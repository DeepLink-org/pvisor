//! Send requests through the Rust cache client without the product CLI.
use anyhow::{Context, bail};
use pvisor::cache::{CacheClient, MAX_READ, Request, Response};
use std::io::{self, Write};

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let mut stdout = io::stdout().lock();
    match args.as_slice() {
        [request] => {
            let request: Request = serde_json::from_str(request).context("decode cache request")?;
            let (response, bytes) = CacheClient::from_env()?.request(request)?;
            if matches!(response, Response::Data { .. }) {
                stdout.write_all(&bytes)?;
            } else {
                serde_json::to_writer(&mut stdout, &response)?;
                writeln!(stdout)?;
            }
        }
        [operation, image, architecture, blobs] if operation == "prepare-with-blobs" => {
            let response = CacheClient::from_env()?.prepare_with_blobs(
                image,
                architecture,
                std::path::Path::new(blobs),
            )?;
            serde_json::to_writer(&mut stdout, &response)?;
            writeln!(stdout)?;
        }
        [operation, handle, path] if operation == "read-file" => {
            let client = CacheClient::from_env()?;
            let mut offset = 0;
            loop {
                let (_, bytes) = client.request(Request::Read {
                    digest: handle.clone(),
                    path: path.as_bytes().to_vec(),
                    offset,
                    length: MAX_READ,
                })?;
                stdout.write_all(&bytes)?;
                if bytes.len() < MAX_READ as usize {
                    break;
                }
                offset += u64::from(MAX_READ);
            }
        }
        _ => bail!(
            "usage: cache_probe REQUEST_JSON | cache_probe read-file HANDLE PATH | cache_probe prepare-with-blobs IMAGE ARCH BLOB_DIR"
        ),
    }
    Ok(())
}
