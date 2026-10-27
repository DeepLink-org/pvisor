//! B-IMAGE-FORMAT-ENG's explicit, ignored storage-only measurement runner.
//! Identical source is compiled against the baseline and replacement backend.
use super::*;
use crate::image::oci::{ImageStore, PreparedImage};
use std::{fs, path::Path, time::Instant};

#[test]
#[ignore = "explicit engineering runner; requires PVISOR_IMAGE_MEASUREMENT plan"]
fn image_format_measurement() {
    run().unwrap();
}

fn run() -> anyhow::Result<()> {
    let plan: serde_json::Value =
        serde_json::from_slice(&fs::read(std::env::var("PVISOR_IMAGE_MEASUREMENT")?)?)?;
    let path = |key: &str| -> anyhow::Result<PathBuf> {
        Ok(PathBuf::from(plan[key].as_str().context("plan path")?))
    };
    let location = path("cache")?;
    let storage = if plan["backend"] == "pool" {
        Storage::Pool(Arc::new(super::super::pool::PoolStorage::open(
            location, true,
        )?))
    } else {
        Storage::filesystem(location, true)?
    };
    let publisher = path("publisher")?;
    let cache = PortableCache::new(storage, Some(publisher.clone()), false);
    let mut result = serde_json::Map::new();
    if plan["action"] == "publish" {
        let store = ImageStore::new(Some(publisher.clone()))?;
        let image = PreparedImage {
            rootfs: path("source")?,
            digest: format!("sha256:{}", "a".repeat(64)),
            env: BTreeMap::new(),
            entrypoint: Vec::new(),
            cmd: Vec::new(),
        };
        let canonical = crate::image::oci::cache_reference("format-bench:test")?.0;
        let start = Instant::now();
        let (response, _) = cache.publish(&store, &image, "amd64", &canonical)?;
        result.insert(
            "publish_ns".into(),
            (start.elapsed().as_nanos() as u64).into(),
        );
        let Response::Prepared { image_handle, .. } = response else {
            bail!("unexpected publication response")
        };
        result.insert("handle".into(), image_handle.into());
    } else {
        let handle = plan["handle"].as_str().context("plan handle")?.to_owned();
        let paths: Vec<Vec<u8>> = serde_json::from_value(plan["paths"].clone())?;
        let source = path("source")?;
        use std::os::unix::ffi::OsStrExt;
        let expected = paths
            .iter()
            .map(|p| fs::read(source.join(std::ffi::OsStr::from_bytes(p))))
            .collect::<Result<Vec<_>, _>>()?;
        let start = Instant::now();
        cache.request(Request::Open {
            handle: handle.clone(),
            architecture: "amd64".into(),
        })?;
        result.insert("open_ns".into(), (start.elapsed().as_nanos() as u64).into());
        for phase in ["cold", "warm"] {
            let start = Instant::now();
            for (path, bytes) in paths.iter().zip(&expected) {
                let (response, _) = cache.request(Request::Stat {
                    digest: handle.clone(),
                    path: path.clone(),
                })?;
                ensure!(
                    matches!(response, Response::Metadata { kind, size, .. } if kind == "file" && size == bytes.len() as u64),
                    "stat oracle failed"
                );
            }
            result.insert(
                format!("{phase}_stat_ns"),
                (start.elapsed().as_nanos() as u64).into(),
            );
        }
        for phase in ["cold", "warm"] {
            let start = Instant::now();
            for (path, expected) in paths.iter().zip(&expected).take(128) {
                let (response, body) = cache.request(Request::Read {
                    digest: handle.clone(),
                    path: path.clone(),
                    offset: 0,
                    length: MAX_READ,
                })?;
                ensure!(
                    body == *expected && matches!(response, Response::Data { .. }),
                    "content oracle failed"
                );
            }
            result.insert(
                format!("{phase}_read_ns"),
                (start.elapsed().as_nanos() as u64).into(),
            );
        }
        let start = Instant::now();
        let mut pending = vec![Vec::new()];
        let mut count = 0u64;
        while let Some(path) = pending.pop() {
            let mut cursor = 0;
            loop {
                let (response, _) = cache.request(Request::List {
                    digest: handle.clone(),
                    path: path.clone(),
                    offset: cursor,
                })?;
                let Response::Entries {
                    names,
                    metadata,
                    next_offset,
                } = response
                else {
                    bail!("list oracle failed")
                };
                ensure!(
                    names.len() == metadata.len(),
                    "directory metadata alignment"
                );
                for (name, item) in names.into_iter().zip(metadata) {
                    let Response::Metadata { kind, .. } = item else {
                        bail!("directory entry oracle failed")
                    };
                    count += 1;
                    if kind == "directory" {
                        let mut child = path.clone();
                        if !child.is_empty() {
                            child.push(b'/');
                        }
                        child.extend_from_slice(&name);
                        pending.push(child);
                    }
                }
                if let Some(next) = next_offset {
                    ensure!(next != cursor, "nonprogressing cursor");
                    cursor = next;
                } else {
                    break;
                }
            }
        }
        ensure!(
            count == plan["entries"].as_u64().context("entry count")?,
            "inventory oracle failed"
        );
        result.insert("scan_ns".into(), (start.elapsed().as_nanos() as u64).into());
        result.insert("entries".into(), count.into());
        #[cfg(target_os = "linux")]
        result.insert(
            "process_status".into(),
            fs::read_to_string("/proc/self/status")?.into(),
        );
    }
    fs::write(
        Path::new(plan["output"].as_str().context("output")?),
        serde_json::to_vec_pretty(&result)?,
    )?;
    Ok(())
}
