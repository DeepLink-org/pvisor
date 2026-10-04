use super::*;
use crate::image::oci::{ImageStore, PreparedImage};
use std::fs;

fn fixture() -> (tempfile::TempDir, PortableCache, ImageStore, PreparedImage) {
    let tmp = tempfile::tempdir().unwrap();
    let cache = PortableCache::new(
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
fn read_handle(cache: &PortableCache) -> String {
    match cache.prepare("example:test", "amd64", false).unwrap().0 {
        Response::Prepared {
            image_handle: Some(handle),
            ..
        } => handle,
        _ => panic!("missing v1 read handle"),
    }
}

#[test]
fn immutable_open_survives_head_replacement_and_does_not_resolve_an_oci_tag() {
    let (tmp, cache, store, image) = fixture();
    let (canonical, _) = crate::image::oci::cache_reference("example:test").unwrap();
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let old = read_handle(&cache);
    let mut updated = image.clone();
    updated.env.insert("EXAMPLE".into(), "new-version".into());
    cache
        .publish(&store, &updated, "amd64", &canonical)
        .unwrap();
    assert_ne!(old, read_handle(&cache));
    fs::remove_file(
        tmp.path()
            .join("shared")
            .join(head_key(&canonical, "linux-amd64")),
    )
    .unwrap();
    fs::remove_dir_all(&store.root).unwrap();
    let reader = PortableCache::new(
        Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
        Some(tmp.path().join("reader")),
        true,
    );
    let (response, _) = reader
        .request(Request::Open {
            handle: old.clone(),
            architecture: "amd64".into(),
        })
        .unwrap();
    assert!(
        matches!(response, Response::Prepared { image_handle: Some(handle), env, .. } if handle == old && env["EXAMPLE"] == "value")
    );
    assert!(
        !tmp.path().join("reader").exists(),
        "immutable open never creates OCI extraction state"
    );
    let (_, bytes) = reader
        .request(Request::Read {
            digest: old.clone(),
            path: b"small".to_vec(),
            offset: 0,
            length: 64,
        })
        .unwrap();
    assert_eq!(bytes, b"small");
    assert!(
        reader
            .request(Request::Open {
                handle: old,
                architecture: "arm64".into()
            })
            .is_err()
    );
    assert!(
        reader
            .request(Request::Open {
                handle: "example:test".into(),
                architecture: "amd64".into()
            })
            .is_err()
    );
}
#[test]
fn filesystem_publishing_and_offline_readers_preserve_the_image_contract() {
    let (tmp, publisher, store, image) = fixture();
    let (canonical, _) = crate::image::oci::cache_reference("example:test").unwrap();
    publisher
        .publish(&store, &image, "amd64", &canonical)
        .unwrap();
    fs::remove_dir_all(&store.root).unwrap();
    let reader = PortableCache::new(
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
            digest: read_handle(&reader),
            path: b"large".to_vec(),
            offset: MAX_READ as u64 - 1,
            length: 25,
        })
        .unwrap();
    assert_eq!(bytes, vec![42; 25]);
    let (_, bytes) = reader
        .request(Request::Read {
            digest: read_handle(&reader),
            path: b"small".to_vec(),
            offset: 100,
            length: 10,
        })
        .unwrap();
    assert!(bytes.is_empty());
    assert!(
        reader
            .request(Request::Read {
                digest: read_handle(&reader),
                path: b"link".to_vec(),
                offset: 0,
                length: 10
            })
            .is_err()
    );
    assert!(
        reader
            .request(Request::Stat {
                digest: read_handle(&reader),
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
                    digest: read_handle(&reader),
                    path: path.into()
                })
                .is_err()
        );
    }
    let inode = |name: &[u8]| match reader
        .request(Request::Stat {
            digest: read_handle(&reader),
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
    let index = cache
        .load(&Handle::parse(&read_handle(&cache)).unwrap())
        .unwrap();
    let content = index
        .content(index.entry(b"small").unwrap().content.unwrap())
        .unwrap();
    let (blob, _) = index.chunk(content.first).unwrap();
    fs::write(
        tmp.path().join("shared").join(data_key(&blob).unwrap()),
        b"corrupt",
    )
    .unwrap();
    assert!(
        cache
            .request(Request::Read {
                digest: read_handle(&cache),
                path: b"small".to_vec(),
                offset: 0,
                length: 5
            })
            .is_err()
    );
    let reader = PortableCache::new(
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
fn directory_pages_preserve_every_host_supported_name() {
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
    // Linux filesystems support arbitrary filename bytes; macOS APFS rejects
    // invalid UTF-8. The binary codec tests cover these bytes on every host.
    if cfg!(target_os = "linux") {
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
    }
    expected.sort();
    let (canonical, _) = crate::image::oci::cache_reference("example:test").unwrap();
    publisher
        .publish(&store, &image, "amd64", &canonical)
        .unwrap();
    fs::remove_dir_all(&store.root).unwrap();
    let reader = PortableCache::new(
        Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
        None,
        true,
    );
    let mut actual = Vec::new();
    let mut offset = 0;
    loop {
        let (response, _) = reader
            .request(Request::List {
                digest: read_handle(&reader),
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
        PortableCache::new(
            Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
            None,
            true,
        )
        .with_local_objects(Some(tmp.path().join("objects")))
    };
    let read = |reader: &PortableCache| {
        reader
            .request(Request::Read {
                digest: read_handle(reader),
                path: b"small".to_vec(),
                offset: 0,
                length: 5,
            })
            .unwrap()
            .1
    };
    let reader = make_reader();
    assert_eq!(read(&reader), b"small");
    let index = reader
        .load(&Handle::parse(&read_handle(&reader)).unwrap())
        .unwrap();
    let content = index
        .content(index.entry(b"small").unwrap().content.unwrap())
        .unwrap();
    let (blob, _) = index.chunk(content.first).unwrap();
    let remote = tmp.path().join("shared").join(data_key(&blob).unwrap());
    let original = fs::read(&remote).unwrap();
    fs::remove_file(&remote).unwrap();
    drop(reader);
    assert_eq!(
        read(&make_reader()),
        b"small",
        "a new reader must reuse local objects"
    );
    fs::write(&remote, &original).unwrap();
    let local = tmp.path().join("objects/immutable").join(&blob[7..]);
    fs::write(&local, b"corrupt").unwrap();
    assert_eq!(read(&make_reader()), b"small");
    assert_eq!(fs::read(local).unwrap(), original);
}

#[test]
fn failed_publication_never_exposes_an_image_reference() {
    let (tmp, publisher, store, image) = fixture();
    fs::create_dir_all(tmp.path().join("shared")).unwrap();
    fs::create_dir(tmp.path().join("outside")).unwrap();
    std::os::unix::fs::symlink(tmp.path().join("outside"), tmp.path().join("shared/data")).unwrap();
    let (canonical, _) = crate::image::oci::cache_reference("example:test").unwrap();
    assert!(
        publisher
            .publish(&store, &image, "amd64", &canonical)
            .is_err()
    );
    assert!(
        !tmp.path()
            .join("shared")
            .join(head_key(&canonical, "linux-amd64"))
            .exists()
    );

    assert_eq!(fs::read_dir(tmp.path().join("outside")).unwrap().count(), 0);
}

#[test]
fn independent_images_share_file_chunks_and_old_revisions_remain_readable() {
    let (tmp, cache, store, image) = fixture();
    let (canonical, _) = crate::image::oci::cache_reference("example:test").unwrap();
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let old = read_handle(&cache);
    let object_paths = || {
        fn walk(root: &std::path::Path, paths: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(root).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, paths);
                } else {
                    paths.push(path);
                }
            }
        }
        let mut paths = Vec::new();
        walk(&tmp.path().join("shared/data"), &mut paths);
        paths.sort();
        paths
    };
    let initial = object_paths();
    let other = crate::image::oci::cache_reference("other:test").unwrap().0;
    cache.publish(&store, &image, "amd64", &other).unwrap();
    cache.publish(&store, &image, "arm64", &canonical).unwrap();
    assert_eq!(
        object_paths(),
        initial,
        "identical files/chunks must be shared across images and platforms"
    );
    assert_ne!(image_key(&other), image_key(&canonical));
    assert!(
        tmp.path()
            .join("shared")
            .join(head_key(&other, "linux-amd64"))
            .exists()
    );
    assert!(
        tmp.path()
            .join("shared")
            .join(head_key(&canonical, "linux-arm64-v8"))
            .exists()
    );
    let mut image = image;
    fs::write(image.rootfs.join("small"), b"changed").unwrap();
    let next_digest = format!("sha256:{}", "b".repeat(64));
    let next_root = store.root.join("rootfs-v3/sha256").join(&next_digest[7..]);
    fs::rename(&image.rootfs, &next_root).unwrap();
    image.rootfs = next_root;
    image.digest = next_digest;
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let new = read_handle(&cache);
    assert_ne!(old, new);
    assert_eq!(object_paths().len(), initial.len() + 1);
    let cold = PortableCache::new(
        Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
        None,
        true,
    );
    for (handle, expected) in [(old, b"small".as_slice()), (new, b"changed".as_slice())] {
        let bytes = cold
            .request(Request::Read {
                digest: handle,
                path: b"small".into(),
                offset: 0,
                length: 64,
            })
            .unwrap()
            .1;
        assert_eq!(bytes, expected);
    }
    assert!(
        cold.request(Request::Stat {
            digest: image.digest,
            path: b"small".into()
        })
        .is_err(),
        "v1 must not introduce a global digest-only index"
    );
}

#[test]
fn stale_publishers_cannot_replace_head_and_corrupt_existing_objects_are_not_reused() {
    let (tmp, cache, store, image) = fixture();
    let canonical = crate::image::oci::cache_reference("example:test")
        .unwrap()
        .0;
    let first = cache.observe(&canonical, "linux-amd64").unwrap();
    let second = cache.observe(&canonical, "linux-amd64").unwrap();
    cache
        .publish_observed(&store, &image, "amd64", &canonical, first)
        .unwrap();
    let head = fs::read(
        tmp.path()
            .join("shared")
            .join(head_key(&canonical, "linux-amd64")),
    )
    .unwrap();
    let error = cache
        .publish_observed(&store, &image, "amd64", &canonical, second)
        .unwrap_err();
    assert!(super::super::storage::is_conflict(&error));
    assert_eq!(
        fs::read(
            tmp.path()
                .join("shared")
                .join(head_key(&canonical, "linux-amd64"))
        )
        .unwrap(),
        head
    );
    let data = tmp
        .path()
        .join("shared")
        .join(data_key(&hash(b"small")).unwrap());
    fs::write(data, b"corrupt").unwrap();
    assert!(
        cache
            .publish(&store, &image, "amd64", &canonical)
            .unwrap_err()
            .to_string()
            .contains("digest mismatch")
    );
    assert_eq!(
        fs::read(
            tmp.path()
                .join("shared")
                .join(head_key(&canonical, "linux-amd64"))
        )
        .unwrap(),
        head
    );
}

#[test]
fn cold_start_fetches_pages_instead_of_deserializing_the_complete_file_tree() {
    let (tmp, cache, store, image) = fixture();
    for i in 0..5000 {
        fs::write(image.rootfs.join("directory").join(format!("f-{i:05}")), []).unwrap();
    }
    let canonical = crate::image::oci::cache_reference("example:test")
        .unwrap()
        .0;
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let handle = read_handle(&cache);
    let reader = PortableCache::new(
        Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
        None,
        true,
    );
    let loaded = reader.load(&Handle::parse(&handle).unwrap()).unwrap();
    assert!(
        loaded.cached_page_count() <= 5,
        "startup must only fetch headers, root metadata and index root"
    );
    assert!(loaded.commit.metadata["index.bin"].bytes > 20 * binary::PAGE_BYTES as u64);
    assert!(matches!(
        reader
            .request(Request::Stat {
                digest: handle.clone(),
                path: b"directory/f-04999".into()
            })
            .unwrap()
            .0,
        Response::Metadata { size: 0, .. }
    ));
    assert!(
        loaded.cached_page_count() <= 10,
        "one lookup must not populate a complete-image index"
    );
    let mut offset = 0;
    let mut count = 0;
    loop {
        let Response::Entries {
            names, next_offset, ..
        } = reader
            .request(Request::List {
                digest: handle.clone(),
                path: b"directory".into(),
                offset,
            })
            .unwrap()
            .0
        else {
            panic!()
        };
        count += names.len();
        let Some(next) = next_offset else { break };
        offset = next;
    }
    assert_eq!(count, 5000);
}

#[test]
fn corrupt_remote_pages_fail_and_corrupt_local_pages_are_refetched() {
    let (tmp, cache, store, image) = fixture();
    let canonical = crate::image::oci::cache_reference("example:test")
        .unwrap()
        .0;
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let handle = read_handle(&cache);
    let parsed = Handle::parse(&handle).unwrap();
    let make = || {
        PortableCache::new(
            Storage::filesystem(tmp.path().join("shared"), false).unwrap(),
            None,
            true,
        )
        .with_local_objects(Some(tmp.path().join("pages")))
    };
    let read = |reader: &PortableCache| {
        reader.request(Request::Stat {
            digest: handle.clone(),
            path: b"small".into(),
        })
    };
    read(&make()).unwrap();
    let local = tmp
        .path()
        .join("pages/pages")
        .join(&parsed.revision)
        .join("index.bin/1");
    fs::write(&local, b"bad").unwrap();
    read(&make()).unwrap();
    assert_eq!(
        fs::metadata(&local).unwrap().len(),
        binary::PAGE_BYTES as u64
    );
    fs::remove_dir_all(tmp.path().join("pages")).unwrap();
    let remote = tmp
        .path()
        .join("shared")
        .join(parsed.prefix())
        .join("index.bin");
    use std::io::{Seek, SeekFrom, Write};
    let mut file = fs::OpenOptions::new().write(true).open(remote).unwrap();
    file.seek(SeekFrom::Start(binary::PAGE_BYTES as u64 + 24))
        .unwrap();
    file.write_all(b"corrupt").unwrap();
    assert!(
        read(&make())
            .unwrap_err()
            .to_string()
            .contains("page digest mismatch")
    );
}

#[test]
fn publication_uses_v1_for_controls_handles_and_binary_magic() {
    let (tmp, cache, store, image) = fixture();
    let canonical = crate::image::oci::cache_reference("example:test")
        .unwrap()
        .0;
    cache.publish(&store, &image, "amd64", &canonical).unwrap();
    let handle = read_handle(&cache);
    assert!(handle.starts_with("pvisor-v1:"));
    let parsed = Handle::parse(&handle).unwrap();
    assert_eq!(parsed.encode(), handle);
    let root = tmp.path().join("shared");
    for key in [
        "format.json".to_string(),
        format!("meta/{}/identity.json", parsed.image_key),
        head_key(&canonical, &parsed.platform),
        format!("{}/COMMIT.json", parsed.prefix()),
        format!("{}/manifest.json", parsed.prefix()),
        format!("{}/config.json", parsed.prefix()),
    ] {
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join(key)).unwrap()).unwrap();
        assert_eq!(record["format_version"], 1);
    }
    for name in BINARY_NAMES {
        let bytes = fs::read(root.join(parsed.prefix()).join(name)).unwrap();
        assert_eq!(&bytes[..8], b"PVICB1\0\0");
        assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 1);
    }
    let checksums = fs::read(root.join(parsed.prefix()).join("checksums.bin")).unwrap();
    assert_eq!(&checksums[..8], b"PVICH1\0\0");
    let old_handle = handle.replacen("pvisor-v1:", "pvisor-v2:", 1);
    assert!(
        cache
            .request(Request::Stat {
                digest: old_handle,
                path: b"small".into(),
            })
            .unwrap_err()
            .to_string()
            .contains("expected immutable pvisor-v1")
    );
    let head_bytes = fs::read(root.join(head_key(&canonical, &parsed.platform))).unwrap();
    let mut head: serde_json::Value = serde_json::from_slice(&head_bytes).unwrap();
    head["format_version"] = 2.into();
    assert!(
        decode_head(
            &serde_json::to_vec(&head).unwrap(),
            &parsed.image_key,
            &parsed.platform,
        )
        .is_err()
    );
    let mut format: serde_json::Value = serde_json::from_slice(FORMAT).unwrap();
    format["format_version"] = 2.into();
    fs::write(
        root.join("format.json"),
        serde_json::to_vec(&format).unwrap(),
    )
    .unwrap();
    assert!(
        cache
            .request(Request::Ping)
            .unwrap_err()
            .to_string()
            .contains("unsupported cache format")
    );
}

#[test]
fn removed_packed_layout_is_not_a_readable_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::filesystem(tmp.path().into(), false).unwrap();
    storage
        .put("v1/format", b"pvisor-cache-v1\n".to_vec(), true)
        .unwrap();
    let cache = PortableCache::new(storage, None, true);
    assert!(
        cache
            .prepare("example:test", "amd64", false)
            .unwrap_err()
            .to_string()
            .contains("image absent from read-only cache")
    );
    assert!(
        cache
            .request(Request::Stat {
                digest: format!("sha256:{}", "a".repeat(64)),
                path: b"small".into(),
            })
            .unwrap_err()
            .to_string()
            .contains("expected immutable pvisor-v1")
    );
}
