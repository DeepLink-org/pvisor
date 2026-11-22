//! Self-contained immutable V2 publications for object storage and static mirrors.
use super::{
    location::Repository,
    portable::MAX_CONTROL,
    storage::{MAX_OBJECT, Storage, StoredObject},
};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    version: u32,
    publication: String,
}

pub(crate) fn validate_name(name: &str) -> anyhow::Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)),
        "invalid local image key"
    );
    Ok(())
}

pub(crate) fn validate_registries(
    registries: &[crate::docker::ImageRegistry],
) -> anyhow::Result<()> {
    ensure!(
        registries.len() <= super::mirrors::MAX_MIRRORS,
        "at most 8 registries are allowed"
    );
    let mut names = std::collections::HashSet::new();
    let mut roots = std::collections::HashSet::new();
    for registry in registries {
        validate_name(&registry.name)?;
        let canonical = match Repository::parse(&registry.location)? {
            Repository::Http(address) => super::http::Http::validate(address)?.to_string(),
            _ => registry.location.trim_end_matches('/').to_string(),
        };
        ensure!(
            names.insert(&registry.name) && roots.insert(canonical),
            "duplicate registry name or location"
        );
    }
    Ok(())
}

pub(crate) fn resolve_repository(
    name: &str,
    registries: &[crate::docker::ImageRegistry],
) -> anyhow::Result<crate::docker::ResolvedImageRepository> {
    validate_name(name)?;
    validate_registries(registries)?;
    ensure!(
        registries.iter().any(|registry| registry.resolve),
        "no registry is authorized to resolve image names"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut selected = None;
    let mut errors = Vec::new();
    for registry in registries.iter().filter(|r| r.resolve) {
        let budget = deadline
            .saturating_duration_since(std::time::Instant::now())
            .min(std::time::Duration::from_secs(5));
        if budget.is_zero() {
            break;
        }
        let result = (|| {
            let storage = super::mirrors::open(&registry.location)?;
            let budget = deadline
                .saturating_duration_since(std::time::Instant::now())
                .min(std::time::Duration::from_secs(5));
            ensure!(!budget.is_zero(), "registry resolution deadline exhausted");
            let object = storage
                .read_timed(&format!("refs/{name}.json"), None, 4096, budget)?
                .context("name not found")?;
            let reference: Reference = serde_json::from_slice(&object.bytes)?;
            ensure!(
                reference.version == 1 && is_publication(&reference.publication),
                "invalid registry reference"
            );
            Ok::<_, anyhow::Error>(reference.publication)
        })();
        match result {
            Ok(publication) => {
                selected = Some(publication);
                break;
            }
            Err(error) => errors.push(format!("{}: {error:#}", registry.name)),
        }
    }
    let publication =
        selected.with_context(|| format!("cannot resolve image {name}: {}", errors.join("; ")))?;
    let repositories: Vec<_> = registries
        .iter()
        .map(|r| format!("{}/{publication}", r.location.trim_end_matches('/')))
        .collect();
    // Once chosen, never inspect another registry's mutable name mapping.
    let template =
        Storage::mirrors(&repositories)?.verified("image.json", None, MAX_CONTROL, |bytes| {
            ensure!(
                super::hash(bytes)[7..] == publication,
                "image publication identity mismatch"
            );
            Ok(())
        })?;
    Ok(crate::docker::ResolvedImageRepository {
        publication,
        repositories,
        template,
    })
}

fn is_publication(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) struct PublicationReference<'a> {
    destination: &'a str,
    storage: Storage,
    key: String,
    previous: Option<StoredObject>,
}

impl<'a> PublicationReference<'a> {
    pub(crate) fn observe(destination: &'a str, name: &str) -> anyhow::Result<Self> {
        validate_name(name)?;
        let storage = Repository::parse(destination)?.open(false)?;
        let key = format!("refs/{name}.json");
        let previous = storage.read(&key, None, 4096)?;
        if let Some(previous) = &previous {
            ensure!(
                previous.version.e_tag.is_some() || previous.version.version.is_some(),
                "registry backend lacks a conditional-write token"
            );
        }
        Ok(Self {
            destination,
            storage,
            key,
            previous,
        })
    }

    pub(crate) fn commit(self, location: &str) -> anyhow::Result<()> {
        let publication = location
            .strip_prefix(&format!("{}/", self.destination.trim_end_matches('/')))
            .context("publication belongs to a different registry")?;
        ensure!(is_publication(publication), "invalid registry publication");
        let bytes = serde_json::to_vec(&Reference {
            version: 1,
            publication: publication.into(),
        })?;
        let matches_publication = || -> anyhow::Result<bool> {
            Ok(self
                .storage
                .read(&self.key, None, 4096)?
                .is_some_and(|value| value.bytes == bytes))
        };
        if self
            .previous
            .as_ref()
            .is_some_and(|value| value.bytes == bytes)
        {
            ensure!(
                matches_publication()?,
                "registry reference changed during publication"
            );
            return Ok(());
        }
        let expected = self.previous.map(|value| value.version);
        if let Err(error) = self
            .storage
            .compare_and_swap(&self.key, bytes.clone(), expected)
        {
            ensure!(
                (!self.storage.is_local() || super::storage::is_conflict(&error))
                    && matches_publication()?,
                "registry reference commit failed: {error:#}"
            );
        }
        Ok(())
    }
}

pub(crate) fn read_repository_template(location: &str) -> anyhow::Result<Vec<u8>> {
    let storage = Repository::parse(location)?.open(true)?;
    let bytes = storage
        .read("image.json", None, MAX_CONTROL)?
        .context("repository has no complete image publication")?
        .bytes;
    // Our publisher uses the template hash as immutable directory identity.
    // Also accept externally maintained static roots without that convention.
    let name = location
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    if name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit()) {
        ensure!(
            &super::hash(&bytes)[7..] == name,
            "image publication identity mismatch"
        );
    }
    Ok(bytes)
}

pub(crate) fn read_repository_config(
    repositories: &[String],
    digest: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    let hex = crate::image::oci::digest_hex(digest)?;
    ensure!(
        !repositories.is_empty() && repositories.len() <= 8,
        "require 1..=8 repositories"
    );
    if repositories.len() > 1 {
        super::mirrors::validate(repositories)?;
    }
    let key = format!("configs/{hex}.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut errors = Vec::new();
    for repository in repositories {
        let result = (|| {
            let storage = super::mirrors::open(repository)?;
            let budget = deadline
                .saturating_duration_since(std::time::Instant::now())
                .min(std::time::Duration::from_secs(5));
            ensure!(!budget.is_zero(), "configuration read deadline exhausted");
            let Some(object) = storage.read_timed(&key, None, MAX_CONTROL, budget)? else {
                return Ok(None);
            };
            ensure!(
                super::hash(&object.bytes) == digest,
                "image config digest mismatch"
            );
            serde_json::from_slice::<serde_json::Value>(&object.bytes)?;
            Ok(Some(object.bytes))
        })();
        match result {
            Ok(Some(bytes)) => return Ok(Some(bytes)),
            Ok(None) => {}
            Err(error) => errors.push(format!("{error:#}")),
        }
    }
    ensure!(
        errors.is_empty(),
        "image configuration read failed: {}",
        errors.join("; ")
    );
    Ok(None)
}

pub(crate) fn publish_repository(
    source: &Path,
    destination: &str,
    template: &[u8],
) -> anyhow::Result<String> {
    validate_destination(destination)?;
    ensure!(
        !template.is_empty() && template.len() <= MAX_CONTROL,
        "invalid image template size"
    );
    let publication = &super::hash(template)[7..];
    let location = format!("{}/{publication}", destination.trim_end_matches('/'));
    let target = Repository::parse(&location)?.open(false)?;
    upload_tree(source, source, &target)?;
    // Sole visibility point: never expose a template before every dependency
    // has been acknowledged/reconciled. Repeated publication is idempotent.
    target.put_verified("image.json", template.to_vec())?;
    Ok(location)
}

pub(crate) fn validate_destination(destination: &str) -> anyhow::Result<()> {
    Repository::parse(destination)?.validate_writable()
}

fn upload_tree(root: &Path, directory: &Path, target: &Storage) -> anyhow::Result<()> {
    let mut entries = std::fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            upload_tree(root, &path, target)?;
        } else {
            ensure!(
                metadata.is_file() && metadata.len() <= MAX_OBJECT as u64,
                "invalid publication object"
            );
            let key = path
                .strip_prefix(root)?
                .to_str()
                .context("non-UTF-8 publication key")?;
            ensure!(
                key != "image.json",
                "publication source contains a visibility marker"
            );
            target.put_verified(key, std::fs::read(&path)?)?;
        }
    }
    Ok(())
}
