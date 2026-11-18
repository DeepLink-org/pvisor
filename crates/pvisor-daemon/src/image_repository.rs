//! Prepared templates, S3/static publication, and the daemon-owned IPFS service.
use anyhow::{Context, ensure};
use clap::{Args, Subcommand};
use fs2::FileExt;
use pvisor_daemon::api::PreparedImage;
use serde::Serialize;
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
pub(super) struct Manage {
    #[arg(long)]
    images_dir: PathBuf,
    #[command(subcommand)]
    command: Operation,
}

#[derive(Subcommand)]
enum Operation {
    Push {
        name: String,
        #[arg(long)]
        to: String,
    },
    Pull {
        name: String,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        refresh: bool,
    },
    Ls,
    Inspect {
        name: String,
    },
}

impl Manage {
    pub(super) fn run(self) -> anyhow::Result<()> {
        let value = match self.command {
            Operation::Ls => {
                let entries = match fs::read_dir(&self.images_dir) {
                    Ok(entries) => Some(entries),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error.into()),
                };
                let mut images = Vec::new();
                for entry in entries.into_iter().flatten() {
                    let path = entry?.path();
                    if path.extension().is_some_and(|ext| ext == "json") {
                        let name = path
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .context("invalid image name")?;
                        validate_name(name)?;
                        let image: PreparedImage = read_json(&path)?;
                        images.push((name.to_owned(), image.pooled_image));
                    }
                }
                images.sort_by(|a, b| a.0.cmp(&b.0));
                serde_json::to_value(images)?
            }
            Operation::Inspect { name } => {
                validate_name(&name)?;
                let image: PreparedImage =
                    read_json(&self.images_dir.join(format!("{name}.json")))?;
                let mut metadata = image.pooled_image.inspect()?;
                metadata["RepoTags"] = serde_json::json!([name]);
                metadata
            }
            Operation::Push { name, to } => {
                let (image, source) = push(&self.images_dir, &name, &to)?;
                serde_json::json!({
                    "source": source, "image_handle": image.cache.handle,
                    "digest": image.digest, "platform": image.platform,
                })
            }
            Operation::Pull {
                name,
                from,
                refresh,
            } => {
                let image = pull(&self.images_dir, &name, from.as_deref(), refresh)?;
                serde_json::json!({
                    "image": name, "source": image.cache.repository, "mirrors": image.cache.mirrors,
                })
            }
        };
        println!("{}", serde_json::to_string(&value)?);
        Ok(())
    }
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

/// Preparation, publication and import serialize on the same image key.
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

fn push(
    images: &Path,
    name: &str,
    destination: &str,
) -> anyhow::Result<(pvisor::docker::Image, String)> {
    use pvisor::docker::Store;
    Store::validate_image_destination(destination)?;
    let _lock = image_lock(images, name)?;
    let image: PreparedImage = read_json(&images.join(format!("{name}.json")))?;
    let portable = serde_json::to_vec(&portable_template(&image))?;
    let source = Store::open(image.rootfs)?.publish_registry_image(
        &image.pooled_image,
        name,
        destination,
        &portable,
    )?;
    Ok((image.pooled_image, source))
}

fn pull(
    images: &Path,
    name: &str,
    source: Option<&str>,
    refresh: bool,
) -> anyhow::Result<pvisor::docker::Image> {
    use pvisor::docker::Store;
    if let Some(source) = source {
        ensure!(
            ["s3://", "http://", "https://"]
                .iter()
                .any(|scheme| source.starts_with(scheme)),
            "--from requires an S3 or HTTP(S) publication"
        );
    }
    let _lock = image_lock(images, name)?;
    let images = images.canonicalize()?;
    let manifest = images.join(format!("{name}.json"));
    ensure!(
        refresh || !manifest.try_exists()?,
        "image already exists; use --refresh"
    );
    let (bytes, mut repositories) = if let Some(source) = source {
        let repository = source.trim_end_matches('/').to_owned();
        (Store::read_image_repository(&repository)?, vec![repository])
    } else {
        let registries = crate::registries::load(&images)?;
        ensure!(
            !registries.is_empty(),
            "image absent locally; configure registries or use --from"
        );
        let resolved = Store::resolve_image_repository(name, &registries)?;
        (resolved.template, resolved.repositories)
    };
    let mut image: PreparedImage = serde_json::from_slice(&bytes)?;
    image.rootfs = images.join("store");
    let store = Store::open(image.rootfs.clone())?;
    image.pooled_image.cache.location = image.rootfs.join("images/pool-v2");
    image.pooled_image.cache.repository = Some(repositories.remove(0));
    image.pooled_image.cache.mirrors = repositories;
    image.library_dir = None;
    store.import_image_config(&image.pooled_image)?;
    drop(store.open_image(&image.pooled_image)?);
    write_json(&manifest, &image, refresh)?;
    Ok(image.pooled_image)
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
        for name in ["registries", "node"] {
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
    fn image_keys_are_confined() {
        for name in ["../escape", ".hidden", "", "a/b"] {
            assert!(validate_name(name).is_err());
        }
        validate_name("python-3.12").unwrap();
    }

    #[test]
    fn records_are_atomic_and_new_imports_cannot_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("image.json");
        write_json(&path, &"original", false).unwrap();
        assert!(write_json(&path, &"updated", false).is_err());
        assert_eq!(read_json::<String>(&path).unwrap(), "original");
        write_json(&path, &"updated", true).unwrap();
        assert_eq!(read_json::<String>(&path).unwrap(), "updated");
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
