use super::*;
use crate::image::oci::{ImageStore, PreparedImage};
use std::fs;

fn fixture() -> (tempfile::TempDir, LegacyCache, ImageStore, PreparedImage) {
    let tmp = tempfile::tempdir().unwrap();
    let cache = LegacyCache::new(
        Storage::filesystem(tmp.path().join("shared"), true).unwrap(),
        Some(tmp.path().join("publisher")),
        false,
    );
    let store = ImageStore::new(Some(tmp.path().join("publisher"))).unwrap();
    let digest = format!("sha256:{}", "a".repeat(64));
    let root = store.root.join("rootfs-v3/sha256").join(&digest[7..]);
    fs::create_dir(&root).unwrap();
    fs::write(root.join("small"), b"small").unwrap();
    fs::write(root.join("large"), vec![42; MAX_READ as usize * 2 + 9]).unwrap();
    std::os::unix::fs::symlink("small", root.join("link")).unwrap();
    fs::hard_link(root.join("small"), root.join("hard")).unwrap();
    fs::create_dir(root.join("directory")).unwrap();
    let image = PreparedImage {
        rootfs: root,
        digest,
        env: BTreeMap::from([("EXAMPLE".into(), "value".into())]),
        entrypoint: vec!["/bin/sh".into()],
        cmd: vec!["-c".into()],
    };
    (tmp, cache, store, image)
}
#[test]
fn filesystem_publishing_and_offline_readers_preserve_the_image_contract() {
    let (tmp, publisher, store, image) = fixture();
    let (canonical, _) = crate::image::oci::cache_reference("example:test").unwrap();
    publisher
        .publish(&store, &image, "amd64", &canonical)
        .unwrap();
    fs::remove_dir_all(&store.root).unwrap();
    let reader = LegacyCache::new(
        Storage::filesystem(tmp.path().join("shared"), true).unwrap(),
        Some(tmp.path().join("reader")),
        true,
    );
    let (response, _) = reader
        .request(Request::Prepare {
            image: "docker.io/library/example:test".into(),
            architecture: "amd64".into(),
            refresh: false,
        })
        .unwrap();
    assert!(
        matches!(response, Response::Prepared { digest, metadata_generation: Some(_), env, .. } if digest == image.digest && env["EXAMPLE"] == "value")
    );
    assert!(
        !tmp.path().join("reader").exists(),
        "warm cache must not initialize local OCI extraction"
    );
    let (_, bytes) = reader
        .request(Request::Read {
            digest: image.digest.clone(),
            path: b"large".to_vec(),
            offset: MAX_READ as u64 - 1,
            length: 25,
        })
        .unwrap();
    assert_eq!(bytes, vec![42; 25]);
    let (_, bytes) = reader
        .request(Request::Read {
            digest: image.digest.clone(),
            path: b"small".to_vec(),
            offset: 100,
            length: 10,
        })
        .unwrap();
    assert!(bytes.is_empty());
    assert!(
        reader
            .request(Request::Read {
                digest: image.digest.clone(),
                path: b"link".to_vec(),
                offset: 0,
                length: 10
            })
            .is_err()
    );
    assert!(
        reader
            .request(Request::Stat {
                digest: image.digest.clone(),
                path: b"link/child".to_vec()
            })
            .is_err()
    );
    for path in [
        b"../small".as_slice(),
        b"/small",
        b"directory/../small",
        b"small\0",
    ] {
        assert!(
            reader
                .request(Request::Stat {
                    digest: image.digest.clone(),
                    path: path.into()
                })
                .is_err()
        );
    }
    let inode = |name: &[u8]| match reader
        .request(Request::Stat {
            digest: image.digest.clone(),
            path: name.into(),
        })
        .unwrap()
        .0
    {
        Response::Metadata { inode, .. } => inode,
        _ => panic!(),
    };
    assert_eq!(inode(b"small"), inode(b"hard"));
}
#[test]
fn corrupt_content_is_not_served_and_read_only_never_prepares_missing_images() {
    let (tmp, cache, store, image) = fixture();
    let (canonical, _) = crate::image::oci::cache_reference("example:test").unwrap();
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let index = cache.for_digest(&image.digest).unwrap();
    let blob = &index.entry(b"small").unwrap().spans[0].blob;
    fs::write(
        tmp.path().join("shared/v1/blobs").join(&blob[7..]),
        b"corrupt",
    )
    .unwrap();
    assert!(
        cache
            .request(Request::Read {
                digest: image.digest.clone(),
                path: b"small".to_vec(),
                offset: 0,
                length: 5
            })
            .is_err()
    );
    let reader = LegacyCache::new(
        Storage::filesystem(tmp.path().join("shared"), true).unwrap(),
        Some(tmp.path().join("reader")),
        true,
    );
    for (name, refresh) in [("example:absent", false), ("example:test", true)] {
        let error = reader
            .request(Request::Prepare {
                image: name.into(),
                architecture: "amd64".into(),
                refresh,
            })
            .unwrap_err();
        assert!(error.to_string().contains("read-only"));
    }
    assert!(!tmp.path().join("reader").exists());
}

#[test]
fn directory_pages_preserve_every_name_including_non_utf8() {
    use std::os::unix::ffi::OsStrExt;
    let (tmp, publisher, store, image) = fixture();
    let mut expected = Vec::new();
    for i in 0..600 {
        let name = format!("entry-{i:04}").into_bytes();
        fs::write(
            image
                .rootfs
                .join("directory")
                .join(std::ffi::OsStr::from_bytes(&name)),
            [],
        )
        .unwrap();
        expected.push(name);
    }
    let name = b"entry-\xff".to_vec();
    fs::write(
        image
            .rootfs
            .join("directory")
            .join(std::ffi::OsStr::from_bytes(&name)),
        [],
    )
    .unwrap();
    expected.push(name);
    expected.sort();
    let (canonical, _) = crate::image::oci::cache_reference("example:test").unwrap();
    publisher
        .publish(&store, &image, "amd64", &canonical)
        .unwrap();
    fs::remove_dir_all(&store.root).unwrap();
    let reader = LegacyCache::new(
        Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
        None,
        true,
    );
    let mut actual = Vec::new();
    let mut offset = 0;
    loop {
        let (response, _) = reader
            .request(Request::List {
                digest: image.digest.clone(),
                path: b"directory".to_vec(),
                offset,
            })
            .unwrap();
        let Response::Entries {
            names,
            metadata: Some(metadata),
            next_offset,
        } = response
        else {
            panic!("missing directory metadata")
        };
        assert!(names.len() <= 256);
        assert_eq!(names.len(), metadata.len());
        assert!(metadata.iter().all(
            |meta| matches!(meta, Response::Metadata { kind, size: 0, .. } if kind == "file")
        ));
        actual.extend(names);
        let Some(next) = next_offset else { break };
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(actual, expected);
}

#[test]
fn local_objects_survive_backend_loss_and_corruption_is_refetched() {
    let (tmp, publisher, store, image) = fixture();
    let (canonical, _) = crate::image::oci::cache_reference("example:test").unwrap();
    publisher
        .publish(&store, &image, "amd64", &canonical)
        .unwrap();
    let make_reader = || {
        LegacyCache::new(
            Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
            None,
            true,
        )
        .with_local_objects(Some(tmp.path().join("objects")))
    };
    let read = |reader: &LegacyCache| {
        reader
            .request(Request::Read {
                digest: image.digest.clone(),
                path: b"small".to_vec(),
                offset: 0,
                length: 5,
            })
            .unwrap()
            .1
    };
    let reader = make_reader();
    assert_eq!(read(&reader), b"small");
    let index = reader.for_digest(&image.digest).unwrap();
    let blob = &index.entry(b"small").unwrap().spans[0].blob;
    let remote = tmp.path().join("shared/v1/blobs").join(&blob[7..]);
    let original = fs::read(&remote).unwrap();
    fs::remove_file(&remote).unwrap();
    drop(reader);
    assert_eq!(
        read(&make_reader()),
        b"small",
        "a new reader must reuse local objects"
    );
    fs::write(&remote, &original).unwrap();
    let local = tmp.path().join("objects/blobs").join(&blob[7..]);
    fs::write(&local, b"corrupt").unwrap();
    assert_eq!(read(&make_reader()), b"small");
    assert_eq!(fs::read(local).unwrap(), original);
}

#[test]
fn failed_publication_never_exposes_an_image_reference() {
    let (tmp, publisher, store, image) = fixture();
    fs::create_dir_all(tmp.path().join("shared/v1")).unwrap();
    fs::create_dir(tmp.path().join("outside")).unwrap();
    std::os::unix::fs::symlink(
        tmp.path().join("outside"),
        tmp.path().join("shared/v1/blobs"),
    )
    .unwrap();
    let (canonical, _) = crate::image::oci::cache_reference("example:test").unwrap();
    assert!(
        publisher
            .publish(&store, &image, "amd64", &canonical)
            .is_err()
    );
    assert!(
        !tmp.path()
            .join("shared")
            .join(reference_key(&canonical, "amd64"))
            .exists()
    );
    assert!(
        !tmp.path()
            .join("shared/v1/images")
            .join(format!("{}.json", &image.digest[7..]))
            .exists()
    );
    assert_eq!(fs::read_dir(tmp.path().join("outside")).unwrap().count(), 0);
}
