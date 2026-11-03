//! Ordered, lazy-initialized mirrors of one immutable publication.
use super::storage::{Storage, StoredObject};
use anyhow::{Context, ensure};
use std::{
    ops::Range,
    sync::Mutex,
    time::{Duration, Instant},
};

pub(super) const MAX_MIRRORS: usize = 8;
const ATTEMPT: Duration = Duration::from_secs(5);
const TOTAL: Duration = Duration::from_secs(30);

pub(crate) struct Mirrors {
    sources: Vec<(String, Mutex<Option<Storage>>)>,
    pid: u32,
}

pub(crate) fn validate(locations: &[String]) -> anyhow::Result<()> {
    ensure!(
        !locations.is_empty() && locations.len() <= MAX_MIRRORS,
        "require 1..=8 ordered repositories"
    );
    let publication = locations[0]
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    ensure!(
        publication.len() == 64 && publication.bytes().all(|b| b.is_ascii_hexdigit()),
        "mirrors require a pinned publication SHA-256"
    );
    for (index, location) in locations.iter().enumerate() {
        ensure!(
            location.trim_end_matches('/').rsplit('/').next() == Some(publication),
            "mirrors must pin the same publication"
        );
        ensure!(!locations[..index].contains(location), "duplicate mirror");
        validate_root(location)?;
    }
    Ok(())
}

pub(crate) fn validate_root(location: &str) -> anyhow::Result<()> {
    if location.starts_with("s3://") {
        super::storage::s3_location(location)?;
    } else {
        ensure!(
            location.starts_with("http://") || location.starts_with("https://"),
            "registries require s3://, http:// or https://"
        );
        super::http::Http::validate(location)?;
    }
    Ok(())
}

pub(crate) fn open(location: &str) -> anyhow::Result<Storage> {
    if location.starts_with("s3://") {
        Storage::s3(location)
    } else {
        Ok(Storage::Http(std::sync::Arc::new(super::http::Http::new(
            location,
        )?)))
    }
}

impl Mirrors {
    pub(super) fn new(locations: &[String]) -> anyhow::Result<Self> {
        validate(locations)?;
        Ok(Self {
            pid: std::process::id(),
            sources: locations
                .iter()
                .map(|s| (s.clone(), Mutex::new(None)))
                .collect(),
        })
    }

    pub(super) fn read(
        &self,
        key: &str,
        range: Option<Range<u64>>,
        limit: usize,
        mut valid: impl FnMut(&[u8]) -> anyhow::Result<()>,
    ) -> anyhow::Result<StoredObject> {
        ensure!(
            self.pid == std::process::id(),
            "reopen mirror client after fork"
        );
        let deadline = Instant::now() + TOTAL;
        let mut errors = Vec::new();
        for (location, slot) in &self.sources {
            let budget = deadline
                .saturating_duration_since(Instant::now())
                .min(ATTEMPT);
            if budget.is_zero() {
                break;
            }
            let result = (|| {
                let storage = {
                    let mut slot = slot
                        .lock()
                        .map_err(|_| anyhow::anyhow!("mirror initialization lock poisoned"))?;
                    if slot.is_none() {
                        *slot = Some(open(location)?);
                    }
                    slot.as_ref().unwrap().clone()
                };
                let budget = deadline
                    .saturating_duration_since(Instant::now())
                    .min(ATTEMPT);
                ensure!(!budget.is_zero(), "mirror read deadline exhausted");
                storage
                    .read_timed(key, range.clone(), limit, budget)?
                    .context("missing repository object")
            })();
            match result {
                Ok(object) => match valid(&object.bytes) {
                    Ok(()) => return Ok(object),
                    Err(error) => errors.push(format!("{location}: {error:#}")),
                },
                Err(error) => errors.push(format!("{location}: {error:#}")),
            }
        }
        anyhow::bail!("all pinned mirrors failed for {key}: {}", errors.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mirror_contract_pins_versions_bounds_sources_and_survives_handoff() {
        let hash = "a".repeat(64);
        let roots = vec![
            format!("https://cdn.example/{hash}"),
            format!("s3://images/{hash}"),
        ];
        validate(&roots).unwrap();
        let mut inherited = Mirrors::new(&roots).unwrap();
        inherited.pid = std::process::id().wrapping_add(1);
        assert!(
            inherited
                .read("format.json", None, 1024, |_| Ok(()))
                .err()
                .unwrap()
                .to_string()
                .contains("after fork")
        );
        assert!(validate(&[]).is_err());
        assert!(validate(&[roots[0].clone(), roots[0].clone()]).is_err());
        assert!(
            validate(&[
                roots[0].clone(),
                format!("https://other.example/{}", "b".repeat(64))
            ])
            .is_err()
        );
        assert!(validate(&[format!("https://user:placeholder@cdn.example/{hash}")]).is_err());
        let too_many: Vec<_> = (0..9)
            .map(|i| format!("https://mirror-{i}.example/{hash}"))
            .collect();
        assert!(validate(&too_many).is_err());
        let client = super::super::CacheClient::from_repositories(&roots).unwrap();
        assert!(client.needs_host_network());
        let binding = client.binding();
        let mut wire = serde_json::to_value(binding).unwrap();
        assert_eq!(wire["mirrors"][0], roots[1]);
        super::super::CacheClient::from_binding(serde_json::from_value(wire.clone()).unwrap())
            .unwrap();
        wire["read_only"] = false.into();
        assert!(
            super::super::CacheClient::from_binding(serde_json::from_value(wire).unwrap()).is_err()
        );
    }
}
