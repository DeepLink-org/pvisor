//! Publish content first, then the index and finally image/reference pointers.
use super::*;
use crate::image::oci::{ImageStore, PreparedImage};
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;

impl LegacyCache {
    pub(in crate::image::cache) fn publish_image(
        &self,
        image: &str,
        architecture: &str,
        refresh: bool,
    ) -> anyhow::Result<Response> {
        ensure!(!self.read_only, "read-only cache cannot publish images");
        ensure!(
            matches!(architecture, "amd64" | "arm64"),
            "unsupported image architecture"
        );
        let (canonical, _) = crate::image::oci::cache_reference(image)?;
        let store = ImageStore::new(self.local_store.clone())?;
        let image = store.prepare_with_refresh(image, architecture, refresh)?;
        self.publish(&store, &image, architecture, &canonical)
            .map(|(response, _)| response)
    }

    pub(super) fn publish(
        &self,
        store: &ImageStore,
        image: &PreparedImage,
        architecture: &str,
        canonical: &str,
    ) -> anyhow::Result<(Response, Vec<u8>)> {
        ensure!(!self.read_only, "cannot publish to a read-only cache");
        let mut entries = Vec::new();
        let mut pending = vec![Vec::new()];
        let mut inodes = HashMap::new();
        let mut pack = Vec::new();
        let mut pack_spans = Vec::new();
        let mut span_count = 0usize;
        let mut totals = ImageTotals::default();
        while let Some(path) = pending.pop() {
            let (metadata, _) = super::super::server::handle(
                store,
                Request::Stat {
                    digest: image.digest.clone(),
                    path: path.clone(),
                },
            )?;
            let Response::Metadata {
                kind, inode, size, ..
            } = &metadata
            else {
                bail!("invalid source metadata")
            };
            let kind = kind.clone();
            let size = *size;
            let next = inodes.len() as u64 + 1;
            let portable_inode = *inodes.entry(*inode).or_insert(next);
            let mut metadata = metadata;
            if let Response::Metadata { inode, .. } = &mut metadata {
                *inode = portable_inode;
            }
            entries.push(Entry {
                path: path.clone(),
                metadata,
                spans: Vec::new(),
            });
            ensure!(
                entries.len() <= MAX_ENTRIES,
                "image exceeds cache entry limit"
            );
            let entry = entries.len() - 1;
            if kind == "directory" {
                let mut children = Vec::new();
                let mut offset = 0;
                loop {
                    let (response, _) = super::super::server::handle(
                        store,
                        Request::List {
                            digest: image.digest.clone(),
                            path: path.clone(),
                            offset,
                        },
                    )?;
                    let Response::Entries {
                        names, next_offset, ..
                    } = response
                    else {
                        bail!("invalid source directory")
                    };
                    for name in names {
                        let mut child = path.clone();
                        if !child.is_empty() {
                            child.push(b'/');
                        }
                        child.extend(name);
                        children.push(child);
                    }
                    let Some(next) = next_offset else { break };
                    offset = next;
                }
                children.reverse();
                pending.extend(children);
            } else if kind == "file" {
                totals.files += 1;
                totals.bytes = totals
                    .bytes
                    .checked_add(size)
                    .context("image totals overflow")?;
                let (parent, name) = super::super::server::parent(store, &image.digest, &path)?;
                let mut file =
                    super::super::server::open_child(&parent, OsStr::from_bytes(&name), false)?;
                ensure!(file.metadata()?.is_file(), "source file type changed");
                let mut remaining = size;
                while remaining > 0 {
                    let length = remaining.min(MAX_READ as u64 - pack.len() as u64) as usize;
                    let offset = pack.len();
                    pack.resize(offset + length, 0);
                    file.read_exact(&mut pack[offset..])
                        .context("source image changed during publication")?;
                    let span = entries[entry].spans.len();
                    entries[entry].spans.push(Span {
                        blob: String::new(),
                        offset: offset as u32,
                        length: length as u32,
                    });
                    pack_spans.push((entry, span));
                    span_count += 1;
                    ensure!(span_count <= MAX_SPANS, "image exceeds cache span limit");
                    remaining -= length as u64;
                    if pack.len() == MAX_READ as usize {
                        self.flush_pack(&mut pack, &mut pack_spans, &mut entries)?;
                    }
                }
                let mut extra = [0u8; 1];
                ensure!(
                    file.read(&mut extra)? == 0,
                    "source file grew during publication"
                );
            }
        }
        self.flush_pack(&mut pack, &mut pack_spans, &mut entries)?;
        let index = Index {
            version: 1,
            digest: image.digest.clone(),
            architecture: architecture.into(),
            env: image.env.clone(),
            entrypoint: image.entrypoint.clone(),
            cmd: image.cmd.clone(),
            totals,
            entries,
        };
        let index = LoadedIndex::validate(index)?;
        let bytes = serde_json::to_vec(&index.index)?;
        ensure!(bytes.len() <= MAX_OBJECT, "cache index exceeds size limit");
        let digest = hash(&bytes);
        self.storage
            .put(&format!("v1/indexes/{}.json", &digest[7..]), bytes, true)?;
        self.storage.put(
            &format!(
                "v1/images/{}.json",
                crate::image::oci::digest_hex(&image.digest)?
            ),
            serde_json::to_vec(&Pointer {
                version: 1,
                index: digest.clone(),
            })?,
            false,
        )?;
        // A reader can discover the new image only after all dependencies exist.
        let reference = Reference {
            version: 1,
            image: canonical.into(),
            architecture: architecture.into(),
            checked_at: now(),
            digest: image.digest.clone(),
            index: digest.clone(),
        };
        self.storage.put(
            &reference_key(canonical, architecture),
            serde_json::to_vec(&reference)?,
            false,
        )?;
        let pinned = canonical
            .rsplit_once('@')
            .context("invalid canonical reference")?
            .0;
        self.storage.put(
            &reference_key(&format!("{pinned}@{}", image.digest), architecture),
            serde_json::to_vec(&Reference {
                image: format!("{pinned}@{}", image.digest),
                ..reference
            })?,
            false,
        )?;
        self.storage
            .put("v1/format", b"pvisor-cache-v1\n".to_vec(), false)?;
        Ok((prepared(&index.index, &digest), Vec::new()))
    }
    fn flush_pack(
        &self,
        pack: &mut Vec<u8>,
        spans: &mut Vec<(usize, usize)>,
        entries: &mut [Entry],
    ) -> anyhow::Result<()> {
        if pack.is_empty() {
            return Ok(());
        }
        let bytes = std::mem::take(pack);
        let digest = hash(&bytes);
        self.storage
            .put(&format!("v1/blobs/{}", &digest[7..]), bytes, true)?;
        for (entry, span) in spans.drain(..) {
            entries[entry].spans[span].blob = digest.clone();
        }
        Ok(())
    }
}
