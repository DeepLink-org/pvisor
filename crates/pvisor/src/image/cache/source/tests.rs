//! Service confinement, protocol, and client/server regression tests.
use super::*;
use crate::image::cache::protocol::{MAX_FRAME, write_frame};
use std::os::unix::fs::symlink;

fn fixture() -> (tempfile::TempDir, ImageStore, String) {
    let tmp = tempfile::tempdir().unwrap();
    let store = ImageStore::new(Some(tmp.path().join("store"))).unwrap();
    let digest = format!("sha256:{}", "a".repeat(64));
    let root = store.root.join("rootfs-v3/sha256").join("a".repeat(64));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("hello"), b"hello world").unwrap();
    fs::create_dir(root.join("dir")).unwrap();
    symlink("hello", root.join("alias")).unwrap();
    symlink("/etc", root.join("escape")).unwrap();
    (tmp, store, digest)
}

#[test]
fn server_metadata_reuses_directory_index_and_invalidates_rebuilt_root() {
    let (_tmp, store, digest) = fixture();
    let first = metadata::directory(&store, &digest, b"").unwrap();
    let again = metadata::directory(&store, &digest, b"").unwrap();
    assert!(
        Arc::ptr_eq(&first, &again),
        "directory must not be scanned again"
    );
    let Response::Metadata { size, .. } = metadata::stat(&store, &digest, b"hello").unwrap() else {
        panic!()
    };
    assert_eq!(size, 11);
    let generation = metadata::generation(&store, &digest).unwrap();
    let root = store.root.join("rootfs-v3/sha256").join(&digest[7..]);
    fs::rename(&root, root.with_extension("old")).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("hello"), b"new").unwrap();
    assert_ne!(generation, metadata::generation(&store, &digest).unwrap());
    assert_eq!(
        &*metadata::directory(&store, &digest, b"").unwrap(),
        &[b"hello".to_vec()]
    );
    let Response::Metadata { size, .. } = metadata::stat(&store, &digest, b"hello").unwrap() else {
        panic!()
    };
    assert_eq!(size, 3);
}

#[test]
fn paths_metadata_pagination_and_eof() {
    let (_tmp, store, digest) = fixture();
    let (response, _) = handle(
        &store,
        Request::List {
            digest: digest.clone(),
            path: vec![],
            offset: 0,
        },
    )
    .unwrap();
    match response {
        Response::Entries {
            names,
            metadata,
            next_offset,
        } => {
            assert_eq!(metadata.as_ref().unwrap().len(), names.len());
            assert_eq!(
                names,
                [
                    b"alias".to_vec(),
                    b"dir".to_vec(),
                    b"escape".to_vec(),
                    b"hello".to_vec()
                ]
            );
            assert!(next_offset.is_none());
        }
        _ => panic!("expected directory"),
    }
    let (response, _) = handle(
        &store,
        Request::Stat {
            digest: digest.clone(),
            path: b"alias".to_vec(),
        },
    )
    .unwrap();
    assert!(
        matches!(response, Response::Metadata { target: Some(target), .. } if target == b"hello")
    );
    for path in [
        b"../hello".as_slice(),
        b"/etc/passwd",
        b"escape/passwd",
        b"alias",
        b"hello\0",
    ] {
        assert!(
            handle(
                &store,
                Request::Read {
                    digest: digest.clone(),
                    path: path.to_vec(),
                    offset: 0,
                    length: 10
                }
            )
            .is_err()
        );
    }
    assert!(
        handle(
            &store,
            Request::Read {
                digest: "sha256:../../etc".into(),
                path: b"passwd".to_vec(),
                offset: 0,
                length: 10
            }
        )
        .is_err()
    );
    assert!(
        handle(
            &store,
            Request::Read {
                digest: digest.clone(),
                path: b"hello".to_vec(),
                offset: 0,
                length: MAX_READ + 1
            }
        )
        .is_err()
    );
    let (_, body) = handle(
        &store,
        Request::Read {
            digest,
            path: b"hello".to_vec(),
            offset: 100,
            length: 10,
        },
    )
    .unwrap();
    assert!(body.is_empty());
}

#[test]
fn directory_metadata_pages_fit_frames_and_old_pages_still_decode() {
    use std::os::unix::ffi::OsStringExt;
    let (_tmp, store, digest) = fixture();
    let root = store.root.join("rootfs-v3/sha256").join(&digest[7..]);
    let target = std::ffi::OsString::from_vec(vec![b'~'; 1000]);
    for index in 0..256 {
        let mut name = format!("{index:03}-").into_bytes();
        name.extend(vec![b'~'; 240]);
        std::os::unix::fs::symlink(&target, root.join(std::ffi::OsString::from_vec(name))).unwrap();
    }
    let mut offset = 0;
    let mut pages = 0;
    loop {
        let (response, _) = handle(
            &store,
            Request::List {
                digest: digest.clone(),
                path: vec![],
                offset,
            },
        )
        .unwrap();
        let mut frame = Vec::new();
        write_frame(&mut frame, &response).unwrap();
        assert!(frame.len() <= MAX_FRAME + 4);
        let Response::Entries {
            names,
            metadata,
            next_offset,
        } = response
        else {
            panic!()
        };
        assert_eq!(metadata.unwrap().len(), names.len());
        assert!(!names.is_empty());
        offset += names.len();
        pages += 1;
        if let Some(next) = next_offset {
            assert_eq!(next, offset);
        } else {
            break;
        }
    }
    assert_eq!(offset, 260);
    assert!(pages > 1);
    let old: Response =
        serde_json::from_str(r#"{"status":"entries","names":[[97]],"next_offset":null}"#).unwrap();
    assert!(matches!(old, Response::Entries { metadata: None, .. }));
}
