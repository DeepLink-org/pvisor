//! `pvisor cache` command definitions and dispatch.
use super::server::serve;
use super::transport::{TOKEN_ENV, endpoint_from_env};
use super::{CacheClient, MAX_READ, Request, architecture};
use crate::image::oci::ImageStore;
use clap::{Args, Subcommand};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct CacheArgs {
    #[command(subcommand)]
    command: CacheCommand,
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    /// Serve cached OCI files (foreground; Unix socket by default).
    Serve {
        /// unix:///absolute/path or tcp://127.0.0.1:PORT. Defaults to CACHE_SERVER.
        #[arg(long)]
        listen: Option<String>,
        /// OCI cache to serve and populate.
        #[arg(long, env = "PVISOR_IMAGE_STORE")]
        image_store: Option<PathBuf>,
    },
    /// Resolve and prepare an image on the server; print its immutable digest.
    Prepare {
        image: String,
        /// Recheck the registry even when a fresh prepared-image record exists.
        #[arg(long)]
        refresh: bool,
    },
    /// List one directory page. Paths are relative to the image root.
    List {
        digest: String,
        path: Option<PathBuf>,
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// Show file attributes without following symlinks.
    Stat { digest: String, path: PathBuf },
    /// Stream one regular file to stdout. Does not follow symlinks.
    Read { digest: String, path: PathBuf },
}

pub fn run(args: CacheArgs) -> anyhow::Result<()> {
    if let CacheCommand::Serve {
        listen,
        image_store,
    } = args.command
    {
        return serve(
            listen.map_or_else(endpoint_from_env, Ok)?,
            ImageStore::new(image_store)?,
            std::env::var(TOKEN_ENV).ok(),
        );
    }
    let client = CacheClient::from_env()?;
    let request = match args.command {
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
