//! `pvisor-cache` command definitions and dispatch.
use pvisor::cache::{CacheBackend, CacheClient, CacheConfig, MAX_READ, Request, serve};
fn architecture() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    }
}
use clap::{Args, Subcommand};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

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
    /// Local OCI staging used by the server or a daemonless cache publisher.
    #[arg(long, global = true)]
    image_store: Option<PathBuf>,
    #[command(subcommand)]
    command: CacheCommand,
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    /// Serve immutable image revisions (foreground; Unix socket by default).
    Serve {
        /// unix:///absolute/path or tcp://127.0.0.1:PORT. Defaults to CACHE_SERVER.
        #[arg(long)]
        listen: Option<String>,
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
    if let CacheCommand::Serve { listen } = args.command {
        anyhow::ensure!(
            config.backend == CacheBackend::Server && !config.read_only,
            "serve requires the server backend; filesystem/S3 backends do not need a daemon"
        );
        return serve(
            listen.unwrap_or(config.location),
            config.image_store,
            std::env::var("PVISOR_CACHE_TOKEN").ok(),
        );
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
        CacheCommand::Serve { .. } => unreachable!(),
    };
    let (response, _) = client.request(request)?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}
