//! Public OCI Distribution client. Image process configuration has one owner:
//! pvisor-guest's launch contract; host extraction is intentionally absent.
use crate::{api::Image, store::ImageRecord};
use anyhow::{Context, ensure};
use pvisor_guest::{GuestConfig, ImageLayer, ProcessConfig, RootDisk, StdinMode};
use reqwest::blocking::{Client, Response};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const ACCEPT: &str = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";
pub(crate) fn hash(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn digest(value: &str) -> anyhow::Result<&str> {
    let hex = value
        .strip_prefix("sha256:")
        .context("only SHA-256 OCI digests are supported")?;
    ensure!(
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid OCI digest"
    );
    Ok(hex)
}
#[derive(Clone, Deserialize)]
struct Descriptor {
    digest: String,
    size: u64,
    #[serde(rename = "mediaType", default)]
    media: String,
    #[serde(default)]
    platform: Option<Platform>,
}
#[derive(Clone, Deserialize)]
struct Platform {
    architecture: String,
    os: String,
}

struct Registry {
    client: Client,
    base: String,
    repo: String,
    token: Option<String>,
}
impl Registry {
    fn get(&mut self, resource: &str, accept: Option<&str>) -> anyhow::Result<Response> {
        let url = format!("{}/v2/{}/{}", self.base, self.repo, resource);
        for attempt in 0..2 {
            let mut req = self.client.get(&url);
            if let Some(accept) = accept {
                req = req.header("Accept", accept);
            }
            if let Some(token) = &self.token {
                req = req.bearer_auth(token);
            }
            let response = req.send()?;
            if response.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                let header = response
                    .headers()
                    .get("WWW-Authenticate")
                    .context("registry did not supply a bearer challenge")?
                    .to_str()?;
                let challenge = header
                    .strip_prefix("Bearer ")
                    .context("only public bearer-auth registries are supported")?;
                let fields: BTreeMap<_, _> = challenge
                    .split(',')
                    .filter_map(|s| {
                        let (key, value) = s.trim().split_once('=')?;
                        Some((key.to_owned(), value.trim_matches('"').to_owned()))
                    })
                    .collect();
                let realm = fields
                    .get("realm")
                    .context("registry challenge has no realm")?;
                let realm_url = reqwest::Url::parse(realm)?;
                ensure!(
                    realm_url.scheme() == "https"
                        || matches!(realm_url.host_str(), Some("127.0.0.1" | "localhost")),
                    "registry token realm must use HTTPS"
                );
                let mut request = self.client.get(realm_url);
                if let Some(service) = fields.get("service") {
                    request = request.query(&[("service", service)]);
                }
                request = request.query(&[("scope", format!("repository:{}:pull", self.repo))]);
                let response = request.send()?.error_for_status()?;
                let body = bounded(response, 1 << 20)?;
                let value: serde_json::Value = serde_json::from_slice(&body)?;
                self.token = value["token"]
                    .as_str()
                    .or(value["access_token"].as_str())
                    .map(str::to_owned);
                ensure!(
                    self.token.as_ref().is_some_and(|t| !t.is_empty()),
                    "registry returned no token"
                );
                continue;
            }
            return Ok(response.error_for_status()?);
        }
        anyhow::bail!("registry authorization failed")
    }
    fn manifest(&mut self, selector: &str) -> anyhow::Result<(Vec<u8>, String)> {
        let body = bounded(
            self.get(&format!("manifests/{selector}"), Some(ACCEPT))?,
            4 << 20,
        )?;
        let actual = format!("sha256:{}", hash(&body));
        if selector.starts_with("sha256:") {
            digest(selector)?;
            ensure!(actual == selector, "manifest digest mismatch");
        }
        Ok((body, actual))
    }
    fn blob(&mut self, root: &Path, desc: &Descriptor) -> anyhow::Result<PathBuf> {
        let path = root.join("blobs").join(digest(&desc.digest)?);
        ensure!(desc.size <= 2u64 << 30, "compressed OCI blob exceeds 2 GiB");
        if path.is_file() {
            verify_file(&path, &desc.digest, desc.size)?;
            return Ok(path);
        }
        let mut response = self.get(&format!("blobs/{}", desc.digest), None)?;
        let mut temporary = tempfile::NamedTempFile::new_in(root.join("blobs"))?;
        let mut hasher = Sha256::new();
        let mut count = 0;
        let mut buffer = [0; 65536];
        loop {
            let size = response.read(&mut buffer)?;
            if size == 0 {
                break;
            }
            count += size as u64;
            ensure!(count <= desc.size, "OCI blob exceeds manifest size");
            hasher.update(&buffer[..size]);
            temporary.write_all(&buffer[..size])?;
        }
        let actual = format!(
            "sha256:{}",
            hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        ensure!(
            count == desc.size && actual == desc.digest,
            "OCI blob digest/size mismatch"
        );
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(&path) {
            Ok(_) => (),
            Err(e) if path.is_file() => {
                drop(e);
                verify_file(&path, &desc.digest, desc.size)?;
            }
            Err(e) => return Err(e.error.into()),
        }
        Ok(path)
    }
}
fn bounded(mut response: Response, max: u64) -> anyhow::Result<Vec<u8>> {
    let mut body = Vec::new();
    response.by_ref().take(max + 1).read_to_end(&mut body)?;
    ensure!(
        body.len() as u64 <= max,
        "registry metadata exceeds size limit"
    );
    Ok(body)
}
fn verify_file(path: &Path, expected: &str, size: u64) -> anyhow::Result<()> {
    ensure!(
        File::open(path)?.metadata()?.len() == size,
        "cached blob size changed"
    );
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = f.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        h.update(&buffer[..n]);
    }
    let actual = format!(
        "sha256:{}",
        h.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    ensure!(actual == expected, "cached OCI blob is corrupt");
    Ok(())
}

pub(crate) fn pull(root: &Path, reference: &str) -> anyhow::Result<ImageRecord> {
    ensure!(
        !reference.is_empty()
            && reference.len() <= 512
            && !reference.contains(['\0', '\\', ' ', '?', '#']),
        "invalid image reference"
    );
    let (registry, repository) = match reference.split_once('/') {
        Some((host, rest)) if host.contains('.') || host.contains(':') || host == "localhost" => {
            (host.to_owned(), rest.to_owned())
        }
        _ => (
            "registry-1.docker.io".into(),
            if reference.contains('/') {
                reference.to_owned()
            } else {
                format!("library/{reference}")
            },
        ),
    };
    let (repo, selector) = if let Some((repo, selector)) = repository.split_once('@') {
        digest(selector)?;
        (repo.to_owned(), selector.to_owned())
    } else if let Some((repo, tag)) = repository.rsplit_once(':') {
        ensure!(!tag.is_empty(), "empty image tag");
        (repo.to_owned(), tag.to_owned())
    } else {
        (repository, "latest".into())
    };
    ensure!(
        registry
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-:".contains(&b)),
        "registry must not contain credentials or URL syntax"
    );
    ensure!(
        !repo.is_empty()
            && repo
                .split('/')
                .all(|c| !c.is_empty() && c != "." && c != ".."),
        "invalid repository"
    );
    let scheme = if registry.starts_with("localhost:") || registry.starts_with("127.0.0.1:") {
        "http"
    } else {
        "https"
    };
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .user_agent("pvisor")
        .build()?;
    let mut registry = Registry {
        client,
        base: format!("{scheme}://{registry}"),
        repo,
        token: None,
    };
    let (mut manifest, mut manifest_digest) = registry.manifest(&selector)?;
    let mut value: serde_json::Value = serde_json::from_slice(&manifest)?;
    if let Some(index) = value.get("manifests") {
        let descriptors: Vec<Descriptor> = serde_json::from_value(index.clone())?;
        let selected = descriptors
            .iter()
            .find(|d| {
                d.platform
                    .as_ref()
                    .is_some_and(|p| p.os == "linux" && p.architecture == "amd64")
            })
            .context("image has no linux/amd64 manifest")?;
        (manifest, manifest_digest) = registry.manifest(&selected.digest)?;
        ensure!(
            manifest.len() as u64 == selected.size,
            "manifest size mismatch"
        );
        value = serde_json::from_slice(&manifest)?;
    }
    let config: Descriptor = serde_json::from_value(value["config"].clone())?;
    ensure!(config.size <= 4 << 20, "image config exceeds 4 MiB");
    let path = registry.blob(root, &config)?;
    let value: serde_json::Value = serde_json::from_reader(File::open(path)?)?;
    ensure!(
        value["architecture"] == "amd64" && value["os"] == "linux",
        "expected linux/amd64 image"
    );
    let process = &value["config"];
    let strings = |key: &str| -> anyhow::Result<Vec<String>> {
        match process.get(key) {
            None | Some(serde_json::Value::Null) => Ok(Vec::new()),
            Some(v) => Ok(serde_json::from_value(v.clone())?),
        }
    };
    let entrypoint = strings("Entrypoint")?;
    let mut argv = entrypoint.clone();
    argv.extend(strings("Cmd")?);
    let mut env = BTreeMap::new();
    for pair in strings("Env")? {
        if let Some((key, value)) = pair.split_once('=') {
            env.insert(key.into(), value.into());
        }
    }
    let descriptors: Vec<Descriptor> = serde_json::from_slice::<serde_json::Value>(&manifest)?
        .get("layers")
        .context("manifest has no layers")
        .and_then(|v| Ok(serde_json::from_value(v.clone())?))?;
    ensure!(descriptors.len() <= 128, "too many OCI layers");
    let mut layers = Vec::new();
    for d in descriptors {
        ensure!(
            matches!(
                d.media.as_str(),
                "application/vnd.oci.image.layer.v1.tar"
                    | "application/vnd.oci.image.layer.v1.tar+gzip"
                    | "application/vnd.docker.image.rootfs.diff.tar.gzip"
            ),
            "this profile supports only tar/gzip OCI layers"
        );
        registry.blob(root, &d)?;
        layers.push(ImageLayer {
            sha256: digest(&d.digest)?.into(),
            media_type: d.media,
        });
    }
    let cwd = process["WorkingDir"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("/");
    let user = process["User"].as_str().unwrap_or("").to_owned();
    let guest = GuestConfig {
        argv,
        env,
        cwd: cwd.into(),
        stdio_ports: Some([false, true, true]),
        process: Some(ProcessConfig {
            user: Some(user),
            new_session: true,
            stdin: StdinMode::Closed,
            attached: true,
            create_workdir: true,
            capabilities: Some(vec![0, 1, 3, 4, 5, 6, 7, 8, 10, 13, 18, 29, 31]),
            ..Default::default()
        }),
        serial_control: true,
        root_disk: Some(RootDisk {
            device: "/dev/vda".into(),
            layers,
        }),
        ..Default::default()
    };
    // An image may rely on a caller-supplied CMD. Other launch fields still
    // validate through the shared contract before its catalog record is published.
    let mut validate = guest.clone();
    if validate.argv.is_empty() {
        validate.argv.push("/bin/true".into());
    }
    validate.command()?;
    ensure!(
        serde_json::to_vec(&validate)?.len() <= 1 << 20,
        "image launch configuration exceeds 1 MiB"
    );
    Ok(ImageRecord {
        summary: Image {
            reference: reference.into(),
            id: config.digest,
            digest: manifest_digest,
        },
        guest,
        entrypoint: entrypoint.len(),
        stop_signal: pvisor_guest::control::parse_signal(
            process["StopSignal"].as_str().unwrap_or("SIGTERM"),
        )?,
    })
}
