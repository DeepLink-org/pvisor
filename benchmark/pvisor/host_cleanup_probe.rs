// B-FS-ENG: identical private-path extension in both frozen cleanup subjects.
#[cfg(test)]
mod cleanup_benchmark {
    use super::*;
    use fuser::{Reply, ReplySender};
    use std::{io::IoSlice, time::Instant};

    struct CheckedReply;
    impl ReplySender for CheckedReply {
        fn send(&self, data: &[IoSlice<'_>]) -> io::Result<()> {
            assert!(data[0].len() >= 16);
            assert_eq!(i32::from_ne_bytes(data[0][4..8].try_into().unwrap()), 0);
            Ok(())
        }
    }

    fn elapsed(name: &str, start: Instant) {
        println!("HOTPATH {name} {}", start.elapsed().as_nanos());
    }

    pub(super) fn extra(overlay: &mut OverlayFs) {
        let inodes: Vec<_> = (0..512)
            .map(|i| {
                let path = PathBuf::from("stat")
                    .join(if i % 2 == 0 { "shallow" } else { "a/b/c/d/e/f" })
                    .join(format!("f{i:04}"));
                overlay.by_path[&path]
            })
            .collect();
        let start = Instant::now();
        for _ in 0..8 {
            for ino in &inodes {
                assert_eq!(overlay.inode_metadata(*ino, None).unwrap().len(), 4096);
            }
        }
        elapsed("warm_getattr", start);

        for count in [128, 2048] {
            let path = PathBuf::from(format!("directory-{count}"));
            let upper = overlay.core.upper().join(&path);
            fs::create_dir(&upper).unwrap();
            for index in (0..count).step_by(4) {
                fs::write(upper.join(format!(".wh.f{index:04}")), b"").unwrap();
            }
            let ino = overlay.inode_for_path_with_metadata(path.clone()).unwrap().0;
            overlay.retain_lookup(ino);
            let start = Instant::now();
            let entries = overlay.directory_snapshot(ino).unwrap();
            elapsed(&format!("directory_snapshot_{count}"), start);
            let expected: Vec<_> = [OsString::from("."), OsString::from("..")]
                .into_iter()
                .chain((0..count).filter(|i| i % 4 != 0).map(|i| OsString::from(format!("f{i:04}"))))
                .collect();
            assert_eq!(entries.iter().map(|e| e.name.clone()).collect::<Vec<_>>(), expected);
            assert!(entries[2..].iter().all(|e| e.ino == 0));
            let nodes_before = overlay.nodes.len();
            let handle = overlay.allocate_handle();
            overlay.open_directories.insert(handle, entries);
            let start = Instant::now();
            overlay.releasedir(ino, handle, 0, ReplyEmpty::new(1, CheckedReply));
            elapsed(&format!("releasedir_lazy_{count}"), start);
            assert_eq!(overlay.nodes.len(), nodes_before);
            assert!(!overlay.open_directories.contains_key(&handle));

            let entries = overlay.directory_snapshot(ino).unwrap();
            let handle = overlay.allocate_handle();
            overlay.open_directories.insert(handle, entries);
            for index in 2..expected.len() {
                let attr = overlay.directory_entry_attr(handle, index).unwrap();
                assert_eq!(attr.size, 4096);
            }
            let children: Vec<_> = overlay.open_directories[&handle][2..].iter().map(|e| e.ino).collect();
            assert_eq!(overlay.nodes.len(), nodes_before + count * 3 / 4);
            let start = Instant::now();
            overlay.releasedir(ino, handle, 0, ReplyEmpty::new(1, CheckedReply));
            elapsed(&format!("releasedir_loaded_{count}"), start);
            assert_eq!(overlay.nodes.len(), nodes_before);
            assert!(!overlay.open_directories.contains_key(&handle));
            for child in children {
                assert!(!overlay.nodes.contains_key(&child));
                assert!(!overlay.by_object.values().any(|v| *v == child));
            }
            assert!(!overlay.by_path.keys().any(|p| p != &path && p.starts_with(&path)));
            for original in &inodes {
                assert!(overlay.nodes.contains_key(original));
            }
        }
    }
}
