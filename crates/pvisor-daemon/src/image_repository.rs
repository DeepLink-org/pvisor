//! Local image policy, S3/static publication, and optional daemon-owned IPFS.
use anyhow::{Context, ensure};
use clap::Args;
use fs2::FileExt;
use pvisor_daemon::runtime::PreparedImage;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

pub(super) const DEFAULT_IMAGES: &str = ".pvisor/images";

#[derive(Args)]
#[command(group(clap::ArgGroup::new("operation").args(["public", "private", "from", "pull"])))]
pub(super) struct Image {
    /// Local image name: inspect if present, otherwise resolve configured registries.
    name: String,
    /// Publish this revision publicly. Already distributed bytes cannot be recalled.
    #[arg(long)]
    public: bool,
    /// Stop retaining this publication; previously shared bytes remain public.
    #[arg(long)]
    private: bool,
    /// Install from a trusted immutable CID, s3:// repository, or HTTP/HTTPS mirror.
    #[arg(long)]
    from: Option<String>,
    /// Resolve configured registries again and atomically refresh this local name.
    #[arg(long)]
    pull: bool,
    /// Publish to a configured registry name, s3:// prefix, or file:/// directory.
    #[arg(long, requires = "public")]
    to: Option<String>,
    /// Image repository; use the same override as prepare-image and serve.
    #[arg(long, default_value = DEFAULT_IMAGES)]
    images_dir: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Distribution {
    handle: String,
    #[serde(alias = "cid")]
    source: String,
    public: bool,
}

pub(super) fn validate_name(name: &str) -> anyhow::Result<()> {
    pvisor::docker::Store::validate_image_key(name)
}

pub(super) fn private_directory(path: &Path) -> anyhow::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "image distribution state must be an owned private directory: {}",
        path.display()
    );
    Ok(())
}

pub(super) fn lock(path: &Path) -> anyhow::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "unsafe image lock"
    );
    file.lock_exclusive()?;
    Ok(file)
}

/// Preparation and visibility changes serialize on the same image key.
pub(super) fn image_lock(images: &Path, name: &str) -> anyhow::Result<File> {
    validate_name(name)?;
    private_directory(&images.join(".distribution"))?;
    let directory = images.join(".distribution/image-locks");
    private_directory(&directory)?;
    lock(&directory.join(format!("{name}.lock")))
}

pub(super) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= 1024 * 1024,
        "invalid image record"
    );
    Ok(serde_json::from_reader(file)?)
}

pub(super) fn write_json(path: &Path, value: &impl Serialize, replace: bool) -> anyhow::Result<()> {
    let parent = path.parent().context("record requires parent")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(file.as_file_mut(), value)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    if replace {
        file.persist(path)?;
    } else {
        file.persist_noclobber(path)?;
    }
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn kubo(repo: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let result = std::process::Command::new("ipfs")
        .env_clear()
        .envs(process_environment(std::env::vars_os()))
        .env("IPFS_PATH", repo)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("run installed Kubo (ipfs must be on PATH)")?;
    ensure!(
        result.status.success(),
        "Kubo failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(result.stdout)
}

fn validate_cid(cid: &str) -> anyhow::Result<()> {
    ensure!(
        (32..=128).contains(&cid.len()) && cid.bytes().all(|b| b.is_ascii_alphanumeric()),
        "invalid immutable CID"
    );
    Ok(())
}

fn address(config: &serde_json::Value, key: &str) -> anyhow::Result<String> {
    let value = config["Addresses"][key]
        .as_str()
        .context("missing Kubo address")?;
    let port = value
        .strip_prefix("/ip4/127.0.0.1/tcp/")
        .context("Kubo API and gateway must bind literal IPv4 loopback")?;
    ensure!(
        port.parse::<u16>().is_ok_and(|p| p != 0),
        "invalid Kubo port"
    );
    Ok(format!("http://127.0.0.1:{port}"))
}

/// Kubo's format remains private implementation state inside the image repository.
/// Only new repositories are configured; existing configurations are never rewritten.
fn ensure_repo(images: &Path) -> anyhow::Result<PathBuf> {
    private_directory(&images.join(".distribution"))?;
    let _lock = lock(&images.join(".distribution/node.lock"))?;
    let repo = images.join(".ipfs");
    private_directory(&repo)?;
    let path = repo.join("config");
    if !path.try_exists()? {
        kubo(&repo, &["init", "--empty-repo"])?;
        let mut config: serde_json::Value = read_json(&path)?;
        // Reserve distinct ports until the complete configuration is written.
        // A later bind race fails startup explicitly, never binds publicly.
        let api = std::net::TcpListener::bind("127.0.0.1:0")?;
        let gateway = std::net::TcpListener::bind("127.0.0.1:0")?;
        config["Addresses"]["API"] =
            format!("/ip4/127.0.0.1/tcp/{}", api.local_addr()?.port()).into();
        config["Addresses"]["Gateway"] =
            format!("/ip4/127.0.0.1/tcp/{}", gateway.local_addr()?.port()).into();
        write_json(&path, &config, true)?;
    }
    let config: serde_json::Value = read_json(&path)?;
    address(&config, "API")?;
    address(&config, "Gateway")?;
    Ok(repo)
}

fn gateway(images: &Path) -> anyhow::Result<String> {
    let config: serde_json::Value = read_json(&images.join(".ipfs/config"))?;
    address(&config, "Gateway")
}

fn distribution(images: &Path, name: &str) -> anyhow::Result<Option<Distribution>> {
    let path = images.join(".distribution").join(format!("{name}.json"));
    match read_json(&path) {
        Ok(record) => Ok(Some(record)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn release_pin(images: &Path, cid: &str) -> anyhow::Result<()> {
    // Another local name may retain the same immutable publication.
    for entry in fs::read_dir(images.join(".distribution"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let record: Distribution = read_json(&path)?;
            if record.public && record.source == cid {
                let name = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .context("invalid image record name")?;
                validate_name(name)?;
                match read_json::<PreparedImage>(&images.join(format!("{name}.json"))) {
                    Ok(image) if image.pooled_image.cache.handle == record.handle => return Ok(()),
                    Ok(_) => {}
                    Err(error)
                        if error
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
    let repo = images.join(".ipfs");
    let pins = kubo(&repo, &["pin", "ls", "--type=recursive", "--quiet"])?;
    if std::str::from_utf8(&pins)?.lines().any(|pin| pin == cid) {
        kubo(&repo, &["pin", "rm", cid])?;
    }
    Ok(())
}

impl Image {
    pub(super) fn run(self) -> anyhow::Result<()> {
        validate_name(&self.name)?;
        let _image_lock = image_lock(&self.images_dir, &self.name)?;
        let images = self.images_dir.canonicalize()?;
        let _catalog_lock = if self.public || self.private {
            Some(lock(&images.join(".distribution/catalog.lock"))?)
        } else {
            None
        };
        let manifest = images.join(format!("{}.json", self.name));
        let resolved = if let Some(source) = &self.from {
            ensure!(!manifest.try_exists()?, "image already exists");
            let repository = if is_object_repository(source) {
                source.trim_end_matches('/').to_string()
            } else {
                validate_cid(source)?;
                ensure_repo(&images)?;
                format!("ipfs+{}/ipfs/{source}", gateway(&images)?)
            };
            let bytes = pvisor::docker::Store::read_image_repository(&repository)?;
            Some((bytes, vec![repository]))
        } else if self.pull || (!manifest.try_exists()? && !self.public && !self.private) {
            let registries = crate::registries::load(&images)?;
            ensure!(
                !registries.is_empty(),
                "image absent locally; configure registries or use --from"
            );
            let resolved =
                pvisor::docker::Store::resolve_image_repository(&self.name, &registries)?;
            Some((resolved.template, resolved.repositories))
        } else {
            None
        };
        if let Some((bytes, mut repositories)) = resolved {
            let mut image: PreparedImage = serde_json::from_slice(&bytes)?;
            image.rootfs = images.join("store");
            let store = pvisor::docker::Store::open(image.rootfs.clone())?;
            image.pooled_image.cache.location = image.rootfs.join("images/pool-v2");
            image.pooled_image.cache.repository = Some(repositories.remove(0));
            image.pooled_image.cache.mirrors = repositories;
            image.library_dir = None;
            // Validate pinned identity/root metadata before local visibility,
            // without fetching the complete image contents.
            drop(store.open_image(&image.pooled_image)?);
            // Install with no public marker: fetching public bytes does not
            // grant permission to publish subsequent local revisions.
            write_json(&manifest, &image, self.pull)?;
        }
        let image: PreparedImage = read_json(&manifest)?;
        let record_path = images
            .join(".distribution")
            .join(format!("{}.json", self.name));
        let previous = distribution(&images, &self.name)?;
        if self.public {
            let destination = crate::registries::destination(&images, self.to.as_deref())?;
            let source = if let Some(destination) = &destination {
                ensure!(
                    destination.starts_with("s3://") || destination.starts_with("file://"),
                    "publication requires s3:// or file:///; HTTP mirrors are read-only"
                );
                let portable = portable_template(&image);
                pvisor::docker::Store::open(image.rootfs.clone())?.publish_registry_image(
                    &image.pooled_image,
                    &self.name,
                    destination,
                    &serde_json::to_vec(&portable)?,
                )?
            } else if let Some(binding) = &image.pooled_image.cache.repository {
                ensure!(
                    binding.starts_with("ipfs+"),
                    "remote S3/HTTP images cannot be republished; use a local prepared image"
                );
                let repo = ensure_repo(&images)?;
                let url = url::Url::parse(
                    binding
                        .strip_prefix("ipfs+")
                        .context("invalid IPFS binding")?,
                )?;
                let cid = url
                    .path()
                    .strip_prefix("/ipfs/")
                    .context("invalid IPFS CID path")?;
                validate_cid(cid)?;
                kubo(&repo, &["pin", "add", cid])?;
                cid.to_string()
            } else {
                let repo = ensure_repo(&images)?;
                let staging = tempfile::tempdir_in(&images)?;
                let pool = staging.path().join("pool");
                pvisor::docker::Store::open(image.rootfs.clone())?
                    .export_image_pool(&image.pooled_image, &pool)?;
                let portable = portable_template(&image);
                write_json(&pool.join("image.json"), &portable, false)?;
                let output = kubo(
                    &repo,
                    &[
                        "add",
                        "--recursive",
                        "--quieter",
                        "--cid-version=1",
                        "--pin=true",
                        pool.to_str().context("non-UTF-8 image directory")?,
                    ],
                )?;
                let cid = std::str::from_utf8(&output)?.trim();
                validate_cid(cid)?;
                cid.to_string()
            };
            write_json(
                &record_path,
                &Distribution {
                    handle: image.pooled_image.cache.handle.clone(),
                    source: source.clone(),
                    public: true,
                },
                true,
            )?;
            if let Some(old) = &previous
                && old.source != source
                && validate_cid(&old.source).is_ok()
            {
                release_pin(&images, &old.source)?;
            }
        } else if self.private
            && let Some(mut record) = previous
        {
            record.public = false;
            write_json(&record_path, &record, true)?;
            if validate_cid(&record.source).is_ok() {
                release_pin(&images, &record.source)?;
            }
        }
        let record = distribution(&images, &self.name)?
            .filter(|record| record.handle == image.pooled_image.cache.handle);
        let source = record
            .as_ref()
            .map(|r| r.source.clone())
            .or_else(|| image.pooled_image.cache.repository.clone());
        let cid = source
            .as_ref()
            .filter(|s| validate_cid(s).is_ok())
            .cloned()
            .or_else(|| {
                source
                    .as_ref()
                    .filter(|s| s.starts_with("ipfs+"))
                    .and_then(|binding| binding.rsplit('/').next().map(String::from))
            });
        println!(
            "{}",
            serde_json::json!({
                "image": self.name,
                "visibility": if record.as_ref().is_some_and(|r| r.public) { "public" } else { "private" },
                "cid": cid,
                "source": source,
                "mirrors": image.pooled_image.cache.mirrors,
            })
        );
        Ok(())
    }
}

fn is_object_repository(source: &str) -> bool {
    source.starts_with("s3://") || source.starts_with("http://") || source.starts_with("https://")
}

fn portable_template(image: &PreparedImage) -> PreparedImage {
    let mut portable = image.clone();
    portable.rootfs = "/pvisor/store".into();
    portable.pooled_image.cache.location = "/pvisor/store/images/pool-v2".into();
    portable.pooled_image.cache.repository = None;
    portable.pooled_image.cache.mirrors.clear();
    portable.library_dir = None;
    portable
}

pub(super) struct Running {
    child: tokio::process::Child,
    output: tokio::task::JoinHandle<()>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.output.abort();
    }
}

pub(super) async fn start(images: &Path) -> anyhow::Result<Running> {
    let images = images.to_owned();
    let repo = tokio::task::spawn_blocking(move || ensure_repo(&images)).await??;
    start_binary(&repo, Path::new("ipfs")).await
}

async fn start_binary(repo: &Path, executable: &Path) -> anyhow::Result<Running> {
    let repo = repo.canonicalize()?;
    let config: serde_json::Value = read_json(&repo.join("config"))?;
    address(&config, "API")?;
    address(&config, "Gateway")?;
    let mut child = Command::new(executable)
        .env_clear()
        .envs(process_environment(std::env::vars_os()))
        .env("IPFS_PATH", repo)
        .args(["daemon", "--enable-gc"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .context("start installed Kubo")?;
    let mut lines = BufReader::new(child.stdout.take().context("missing Kubo output")?).lines();
    let ready = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(line) = lines.next_line().await? {
            eprintln!("ipfs: {line}");
            if line.trim() == "Daemon is ready" {
                return Ok::<_, anyhow::Error>(());
            }
        }
        anyhow::bail!("Kubo exited before readiness")
    })
    .await;
    if !matches!(&ready, Ok(Ok(()))) {
        let _ = child.kill().await;
        ready.context("Kubo readiness timed out")??;
    }
    let output = tokio::spawn(async move {
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("ipfs: {line}");
        }
    });
    Ok(Running { child, output })
}

pub(super) async fn wait(service: &mut Option<Running>) -> anyhow::Result<()> {
    if let Some(service) = service {
        let status = service.child.wait().await?;
        anyhow::bail!("IPFS component exited unexpectedly: {status}");
    }
    std::future::pending().await
}

pub(super) async fn stop(service: &mut Running) -> anyhow::Result<()> {
    if service.child.try_wait()?.is_none() {
        service.child.start_kill()?;
    }
    service.child.wait().await?;
    Ok(())
}

fn process_environment(
    env: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    env.into_iter()
        .filter(|(key, _)| {
            ["PATH", "HOME", "TMPDIR", "TMP", "TEMP"]
                .iter()
                .any(|allowed| key == *allowed)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kubo_does_not_inherit_keys_or_an_ambient_api_override() {
        let env = process_environment(
            [
                "PATH",
                "HOME",
                "IPFS_API",
                "IPFS_PATH",
                "OPEN_SANDBOX_API_KEY",
                "E2B_API_KEY",
            ]
            .into_iter()
            .map(|key| (key.into(), "value".into())),
        );
        assert_eq!(
            env.iter()
                .map(|(key, _)| key.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["PATH", "HOME"]
        );
    }

    #[test]
    fn image_keys_cannot_collide_with_global_lock_namespaces() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["catalog", "registries", "node"] {
            let _image = image_lock(temp.path(), name).unwrap();
            let global = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(
                    temp.path()
                        .join(".distribution")
                        .join(format!("{name}.lock")),
                )
                .unwrap();
            global
                .try_lock_exclusive()
                .expect("per-image and global locks must be different files");
        }
    }

    #[test]
    fn image_keys_are_confined_and_private_is_the_default() {
        for name in ["../escape", ".hidden", "", "a/b"] {
            assert!(validate_name(name).is_err());
        }
        validate_name("python-3.12").unwrap();
        let tmp = tempfile::tempdir().unwrap();
        assert!(distribution(tmp.path(), "python").unwrap().is_none());
        assert!(!tmp.path().join(".ipfs").exists());
    }

    #[test]
    fn records_are_atomic_and_new_imports_cannot_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let record = Distribution {
            handle: "revision".into(),
            source: "b".repeat(59),
            public: false,
        };
        let path = tmp.path().join("image.json");
        write_json(&path, &record, false).unwrap();
        assert!(write_json(&path, &record, false).is_err());
        assert!(!read_json::<Distribution>(&path).unwrap().public);
        let record = Distribution {
            public: true,
            ..record
        };
        write_json(&path, &record, true).unwrap();
        assert!(read_json::<Distribution>(&path).unwrap().public);
    }

    #[tokio::test]
    async fn node_rejects_public_management_bind_before_spawning() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("config"),
            r#"{"Addresses":{"API":"/ip4/0.0.0.0/tcp/5001","Gateway":"/ip4/127.0.0.1/tcp/8081"}}"#,
        )
        .unwrap();
        assert!(start_binary(tmp.path(), Path::new("ipfs")).await.is_err());
    }

    #[tokio::test]
    async fn node_ownership_readiness_exit_and_reaping() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("config"), r#"{"Addresses":{"API":"/ip4/127.0.0.1/tcp/5001","Gateway":"/ip4/127.0.0.1/tcp/8081"}}"#).unwrap();
        let executable = tmp.path().join("fake-kubo");
        let write_script = |body: &str| {
            fs::write(
                &executable,
                format!(
                    "#!/bin/sh\n[ \"$1\" = daemon ] && [ \"$2\" = --enable-gc ] || exit 9\n{body}\n"
                ),
            )
            .unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        };
        write_script("printf 'Daemon is ready\\n'\nexec sleep 60");
        let mut service = start_binary(tmp.path(), &executable).await.unwrap();
        assert!(service.child.try_wait().unwrap().is_none());
        stop(&mut service).await.unwrap();
        assert!(service.child.try_wait().unwrap().is_some());
        drop(service);
        write_script("printf 'Daemon is ready\\n'\nexit 7");
        let mut service = Some(start_binary(tmp.path(), &executable).await.unwrap());
        assert!(
            wait(&mut service)
                .await
                .unwrap_err()
                .to_string()
                .contains("exited unexpectedly")
        );
        drop(service);
        write_script("exit 7");
        assert!(start_binary(tmp.path(), &executable).await.is_err());
    }
}
