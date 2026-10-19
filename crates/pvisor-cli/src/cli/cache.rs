//! `pvisor cache` command definitions and dispatch.
use clap::{Args, Subcommand};
use pvisor::cache::{CacheBackend, CacheClient, CacheConfig, MAX_READ, Request};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

fn architecture() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    }
}

#[derive(Debug, Args)]
pub struct CacheArgs {
    /// Cache backend: daemon, shared filesystem, or direct S3 object storage.
    #[arg(long, global = true, value_parser = super::values::cache_backend())]
    backend: Option<CacheBackend>,
    /// Server endpoint, absolute shared directory, or s3://BUCKET/PREFIX.
    #[arg(long, global = true)]
    location: Option<String>,
    /// Read published images without registry access or cache writes.
    #[arg(long, global = true)]
    read_only: bool,
    /// Local OCI staging used by a filesystem/S3 cache publisher.
    #[arg(long, global = true)]
    image_store: Option<PathBuf>,
    #[command(subcommand)]
    command: CacheCommand,
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    /// Copy a quiescent local rootfs into a durable, private VM base catalog.
    PrepareRootfs {
        /// Independent source tree; never the live host root or the base store.
        #[arg(long)]
        rootfs: PathBuf,
        /// Private SnapshotStore, separate from every guest-visible workspace.
        #[arg(long)]
        base_store: PathBuf,
    },
    /// Copy a quiescent local workspace into a durable private workspace catalog.
    PrepareWorkspace {
        /// Fixed read source; the future Run still applies to a live workspace.
        #[arg(long)]
        workspace: PathBuf,
        /// Private SnapshotStore, independent of the rootfs store and Run backing.
        #[arg(long)]
        base_store: PathBuf,
    },
    /// Resolve an image; print its manifest digest and immutable read handle.
    Prepare {
        image: String,
        /// Recheck the registry even when a fresh prepared-image record exists.
        #[arg(long)]
        refresh: bool,
    },
    /// Unpack an OCI image and publish its file index and content blocks to S3 or a filesystem.
    Publish {
        image: String,
        /// Target platform architecture (defaults to the host architecture).
        #[arg(long, value_parser = ["amd64", "arm64"])]
        architecture: Option<String>,
        /// Recheck the registry instead of reusing the local prepared image.
        #[arg(long)]
        refresh: bool,
    },
    /// List one directory page. Paths are relative to the image root.
    List {
        /// Immutable image_handle from prepare/publish.
        digest: String,
        path: Option<PathBuf>,
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// Show file attributes without following symlinks.
    Stat {
        /// Immutable image_handle from prepare/publish.
        digest: String,
        path: PathBuf,
    },
    /// Stream one regular file to stdout. Does not follow symlinks.
    Read { digest: String, path: PathBuf },
}

pub fn run(args: CacheArgs) -> anyhow::Result<()> {
    let prepared_source = match &args.command {
        CacheCommand::PrepareRootfs { rootfs, base_store } => Some(("rootfs", rootfs, base_store)),
        CacheCommand::PrepareWorkspace {
            workspace,
            base_store,
        } => Some(("workspace", workspace, base_store)),
        _ => None,
    };
    if let Some((purpose, source, base_store)) = prepared_source {
        anyhow::ensure!(
            args.backend.is_none()
                && args.location.is_none()
                && !args.read_only
                && args.image_store.is_none(),
            "prepare-{purpose} is local and cannot use cache backend/location/read-only/image-store options"
        );
        let source = source.canonicalize()?;
        // Check before SnapshotStore::new creates/chmods directories.
        let store_path = super::run::canonical_candidate(base_store)?;
        super::host_service::reject_guest_exposure([source.clone(), store_path.clone()])?;
        anyhow::ensure!(
            !source.starts_with(&store_path) && !store_path.starts_with(&source),
            "prepared {purpose} source and store must be independent trees"
        );
        let store = pvisor::environment_snapshot::SnapshotStore::new(&store_path)?;
        let base = if purpose == "rootfs" {
            store.prepare_rootfs(&source)?
        } else {
            store.prepare_workspace(&source)?
        };
        let selector = pvisor::PreparedBase {
            store: store_path,
            base: base.reference().clone(),
        };
        let mut output = serde_json::Map::new();
        output.insert("schema_version".into(), 1.into());
        output.insert(
            format!("prepared_{purpose}"),
            serde_json::to_value(selector)?,
        );
        output.insert(purpose.into(), serde_json::to_value(base.root())?);
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }
    let config = CacheConfig::from_options(
        args.backend,
        args.location,
        args.read_only.then_some(true),
        args.image_store,
    )?;
    if matches!(args.command, CacheCommand::Publish { .. }) {
        let address = config.address()?;
        anyhow::ensure!(
            address.starts_with("s3://") || address.starts_with("file://"),
            "publish requires a filesystem or S3 backend; use --backend s3 --location s3://BUCKET/PREFIX"
        );
        anyhow::ensure!(!config.read_only, "read-only cache cannot publish images");
    }
    let client = CacheClient::from_config(config)?;
    let request = match args.command {
        CacheCommand::Publish {
            image,
            architecture: target,
            refresh,
        } => {
            let response =
                client.publish(&image, target.as_deref().unwrap_or(architecture()), refresh)?;
            println!("{}", serde_json::to_string_pretty(&response)?);
            return Ok(());
        }
        CacheCommand::Prepare { image, refresh } => Request::Prepare {
            image,
            architecture: architecture().into(),
            refresh,
        },
        CacheCommand::List {
            digest,
            path,
            offset,
        } => Request::List {
            digest,
            path: path.unwrap_or_default().as_os_str().as_bytes().to_vec(),
            offset,
        },
        CacheCommand::Stat { digest, path } => Request::Stat {
            digest,
            path: path.as_os_str().as_bytes().to_vec(),
        },
        CacheCommand::Read { digest, path } => {
            let mut offset = 0;
            let mut stdout = std::io::stdout().lock();
            loop {
                let (_, body) = client.request(Request::Read {
                    digest: digest.clone(),
                    path: path.as_os_str().as_bytes().to_vec(),
                    offset,
                    length: MAX_READ,
                })?;
                stdout.write_all(&body)?;
                offset += body.len() as u64;
                if body.len() < MAX_READ as usize {
                    break;
                }
            }
            return Ok(());
        }
        CacheCommand::PrepareRootfs { .. } | CacheCommand::PrepareWorkspace { .. } => {
            unreachable!()
        }
    };
    let (response, _) = client.request(request)?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}
