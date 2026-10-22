// B-FS-ENG: appended identically to frozen fs.rs copies, never to product sources.
#[cfg(test)]
mod hotpath_benchmark {
    use super::*;
    use fuser::{Reply, ReplySender};
    use pvisor_overlay_core::{LayerMutability, OverlayLayout};
    use std::{
        io::IoSlice,
        sync::atomic::{AtomicUsize, Ordering},
        time::Instant,
    };

    struct CheckedReply(Arc<AtomicUsize>);
    impl ReplySender for CheckedReply {
        fn send(&self, data: &[IoSlice<'_>]) -> io::Result<()> {
            assert!(data[0].len() >= 16);
            assert_eq!(i32::from_ne_bytes(data[0][4..8].try_into().unwrap()), 0);
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }
    fn elapsed(name: &str, start: Instant) {
        println!("HOTPATH {name} {}", start.elapsed().as_nanos());
    }
    #[test]
    #[ignore = "explicit frozen release phase benchmark; no real mount"]
    fn probe() {
        let lower = PathBuf::from(std::env::var_os("HOTPATH_LOWER").unwrap());
        let stage = PathBuf::from(std::env::var_os("HOTPATH_STAGE").unwrap());
        let compact = std::env::var("HOTPATH_CONDITION").unwrap() == "mutable-compact";
        let journal = compact.then(|| stage.join("preimages"));
        let layout = OverlayLayout::new(vec![lower.clone()], lower)
            .unwrap()
            .with_lower_mutability(vec![if compact {
                LayerMutability::Mutable
            } else {
                LayerMutability::Immutable
            }])
            .unwrap();
        let core = OverlayCore::new_for_layout_with_compact_preimages(
            layout,
            stage.join("upper"),
            Some(stage.join("work")),
            vec![],
            journal.clone(),
        )
        .unwrap();
        let mut overlay = OverlayFs::from_core(core).unwrap();
        let stat_paths: Vec<_> = (0..512)
            .map(|i| {
                PathBuf::from("stat")
                    .join(if i % 2 == 0 { "shallow" } else { "a/b/c/d/e/f" })
                    .join(format!("f{i:04}"))
            })
            .collect();
        let mut inodes = Vec::with_capacity(512);
        let start = Instant::now();
        for path in &stat_paths {
            let (ino, metadata) = overlay.inode_for_path_with_metadata(path.clone()).unwrap();
            assert_eq!(metadata.len(), 4096);
            inodes.push(ino);
        }
        elapsed("lookup", start);
        let start = Instant::now();
        for ino in inodes {
            assert_eq!(overlay.inode_metadata(ino, None).unwrap().len(), 4096);
        }
        elapsed("getattr", start);
        for kind in ["append", "truncate"] {
            let inodes: Vec<_> = (0..128)
                .map(|i| {
                    overlay
                        .inode_for_path_with_metadata(PathBuf::from(format!("{kind}/f{i:04}")))
                        .unwrap()
                        .0
                })
                .collect();
            let flags = libc::O_WRONLY
                | if kind == "append" {
                    libc::O_APPEND
                } else {
                    libc::O_TRUNC
                };
            let start = Instant::now();
            for ino in inodes {
                let (mut file, _) = overlay.open_inode_with_backing(ino, flags).unwrap();
                use io::Write;
                file.write_all(b"CHANGED!").unwrap();
            }
            elapsed(&format!("{kind}_open_write_close"), start);
        }
        for kind in ["mkdir", "fifo", "symlink"] {
            let parent_path = PathBuf::from(format!("created-{kind}"));
            overlay.core.create_dir(&parent_path, 0o755).unwrap();
            let parent = overlay.inode_for_path_with_metadata(parent_path).unwrap().0;
            let names: Vec<_> = (0..128)
                .map(|i| OsString::from(format!("f{i:04}")))
                .collect();
            let sent = Arc::new(AtomicUsize::new(0));
            let start = Instant::now();
            for name in &names {
                let reply = ReplyEntry::new(1, CheckedReply(sent.clone()));
                match kind {
                    "mkdir" => overlay.mkdir(parent, name, 0o755, 0, reply),
                    "fifo" => {
                        overlay.mknod(parent, name, libc::S_IFIFO as u32 | 0o600, 0, 0, reply)
                    }
                    "symlink" => {
                        overlay.symlink(parent, name, Path::new("unchanged-target"), reply)
                    }
                    _ => unreachable!(),
                }
            }
            elapsed(kind, start);
            assert_eq!(sent.load(Ordering::Relaxed), 128);
        }
        drop(overlay);
        if let Some(journal) = journal {
            let observations = pvisor_overlay_core::load_preimages(&journal).unwrap();
            for kind in ["append", "truncate"] {
                for i in 0..128 {
                    let path = format!("{kind}/f{i:04}");
                    assert!(observations.iter().any(|o| o.path == path.as_bytes()));
                }
            }
            fs::write(
                stage.join("observation-count.json"),
                observations.len().to_string(),
            )
            .unwrap();
        }
    }
}
