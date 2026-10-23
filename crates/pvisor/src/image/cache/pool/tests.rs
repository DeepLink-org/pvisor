use super::*;
use pvisor_overlay_core::{OverlayCore, backend};
use std::{collections::BTreeMap, fs};

#[test]
fn import_rejects_unrepresentable_xattrs_without_publishing_a_revision() {
    let temporary = tempfile::tempdir().unwrap();
    let source = ImageStore::new(Some(temporary.path().join("source"))).unwrap();
    let digest = format!("sha256:{}", "d".repeat(64));
    let rootfs = source.root.join("rootfs-v3/sha256").join(&digest[7..]);
    fs::create_dir(&rootfs).unwrap();
    fs::write(rootfs.join("file"), b"keep attributes").unwrap();
    pvisor_overlay_core::sys::set_xattr(
        &rootfs.join("file"),
        std::ffi::OsStr::new("user.required"),
        b"preserve",
        0,
    )
    .unwrap();
    let image = PreparedImage {
        rootfs,
        digest,
        env: BTreeMap::new(),
        entrypoint: vec![],
        cmd: vec![],
    };
    let pool = temporary.path().join("pool");
    let error = publish_local_image(&pool, &source, &image, "example:xattr", "amd64").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not support xattr user.required")
    );
    assert!(!pool.join("meta").exists());
}

#[test]
fn encoding_roundtrips_bounds_and_rejects_corruption_without_changing_raw_cache() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("pool");
    let pool = Storage::Pool(Arc::new(PoolStorage::open(root.clone(), true).unwrap()));
    for bytes in [vec![42; super::super::MAX_READ as usize], (0..37).collect()] {
        let digest = super::super::hash(&bytes);
        let key = format!(
            "data/sha256/{}/{}/{}",
            &digest[7..9],
            &digest[9..11],
            &digest[7..]
        );
        pool.put(&key, bytes.clone(), true).unwrap();
        pool.put(&key, bytes.clone(), true).unwrap();
        assert_eq!(pool.get(&key).unwrap().unwrap(), bytes);
        assert_eq!(pool.range(&key, 1..4).unwrap(), bytes[1..4]);
        let encoded = fs::read(root.join(&key)).unwrap();
        assert_eq!(encoded[8], if bytes.len() > 100 { 1 } else { 0 });
        if bytes.len() > 100 {
            assert!(encoded.len() < bytes.len() / 10);
        }
        let mut corrupt = encoded.clone();
        corrupt[9..13].copy_from_slice(&u32::MAX.to_le_bytes());
        fs::write(root.join(&key), corrupt).unwrap();
        assert!(pool.get(&key).is_err());
        let mut corrupt = encoded.clone();
        *corrupt.last_mut().unwrap() ^= 0xff;
        fs::write(root.join(&key), corrupt).unwrap();
        assert!(pool.get(&key).is_err());
        fs::write(root.join(&key), encoded).unwrap();
    }
    let raw = temporary.path().join("raw");
    let storage = Storage::filesystem(raw.clone(), true).unwrap();
    storage
        .put("format.json", b"existing V1".to_vec(), true)
        .unwrap();
    assert!(PoolStorage::open(raw, true).is_err());
    assert!(PoolStorage::open(root, false).is_ok());
}

#[test]
fn images_share_content_survive_source_removal_and_copy_up_without_raw_mirror() {
    let temporary = tempfile::tempdir().unwrap();
    let location = temporary.path().join("pool");
    let source = ImageStore::new(Some(temporary.path().join("source"))).unwrap();
    let scratch = temporary.path().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let bytes = vec![17; super::super::MAX_READ as usize + 123];
    let mut handles = Vec::new();
    for (revision, reference) in [('a', "example:first"), ('b', "example:second")] {
        let digest = format!("sha256:{}", revision.to_string().repeat(64));
        let rootfs = source.root.join("rootfs-v3/sha256").join(&digest[7..]);
        fs::create_dir(&rootfs).unwrap();
        fs::write(rootfs.join("file"), &bytes).unwrap();
        fs::hard_link(rootfs.join("file"), rootfs.join("alias")).unwrap();
        std::os::unix::fs::symlink("file", rootfs.join("link")).unwrap();
        let image = PreparedImage {
            rootfs,
            digest,
            env: BTreeMap::new(),
            entrypoint: vec![],
            cmd: vec![],
        };
        handles.push((
            publish_local_image(&location, &source, &image, reference, "amd64").unwrap(),
            image.digest,
        ));
    }
    fs::remove_dir_all(&source.root).unwrap();
    for (handle, digest) in handles {
        let raw = CacheClient::new(format!("file://{}", location.display()), None).unwrap();
        assert!(
            raw.request(Request::Open {
                handle: handle.clone(),
                architecture: "amd64".into()
            })
            .is_err(),
            "raw V1 readers must reject the compressed pool namespace"
        );
        assert!(open_local_image(&location, &handle, "arm64", &digest, &scratch).is_err());
        assert!(
            open_local_image(
                &location,
                &handle,
                "amd64",
                &format!("sha256:{}", "c".repeat(64)),
                &scratch
            )
            .is_err()
        );
        let image = open_local_image(&location, &handle, "amd64", &digest, &scratch).unwrap();
        let path = image.root().join("file");
        assert_eq!(
            backend::read_at(&path, super::super::MAX_READ as u64 - 2, 5)
                .unwrap()
                .unwrap(),
            vec![17; 5]
        );
        assert!(!scratch.join("unused-block-cache").exists());
        backend::prepare_metadata(&path).unwrap();
        backend::prepare_metadata(&image.root().join("alias")).unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::metadata(&path).unwrap().ino(),
            fs::metadata(image.root().join("alias")).unwrap().ino()
        );
        assert_eq!(
            backend::read_link(image.root().join("link")).unwrap(),
            Path::new("file")
        );
        let upper = temporary.path().join(format!("upper-{handle}"));
        let core = OverlayCore::new(vec![image.root().into()], upper, None).unwrap();
        let writable = core.copy_up(Path::new("file")).unwrap();
        assert_eq!(fs::read(&writable).unwrap(), bytes);
        fs::write(writable, b"changed").unwrap();
        assert_eq!(backend::read_at(&path, 0, 5).unwrap().unwrap(), vec![17; 5]);
        let projection = image.root().parent().unwrap().to_owned();
        drop(core);
        drop(image);
        assert!(!projection.exists());
    }
    fn objects(path: &Path) -> Vec<PathBuf> {
        fs::read_dir(path)
            .unwrap()
            .flat_map(|entry| {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    objects(&path)
                } else {
                    vec![path]
                }
            })
            .collect()
    }
    assert_eq!(
        objects(&location.join("data")).len(),
        2,
        "two revisions share the same two content objects"
    );
}
