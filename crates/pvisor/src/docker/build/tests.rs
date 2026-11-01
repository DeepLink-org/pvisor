use super::*;
use std::os::unix::fs::symlink;

#[test]
fn draft_records_cannot_be_read_as_published_images() {
    let work = tempfile::tempdir().unwrap();
    let draft = scratch(work.path()).unwrap();
    let record = serde_json::to_value(&draft.image).unwrap();
    assert!(serde_json::from_value::<Image>(record).is_err());
}

fn options(context: &Path) -> BuildOptions {
    BuildOptions {
        context: context.into(),
        file: context.join("Dockerfile"),
        tags: vec!["example:test".into()],
        build_args: BTreeMap::new(),
        target: None,
        platform: None,
        pull: false,
        no_cache: false,
        cpus: 1,
        memory_mib: 256,
        network: false,
        timeout: Duration::from_secs(30),
        quiet: true,
        output: None,
    }
}

fn checkpoint_fixture() -> (tempfile::TempDir, Store, Built) {
    let work = tempfile::tempdir().unwrap();
    let store = Store::open(work.path().join("store")).unwrap();
    let context = work.path().join("context");
    fs::create_dir(&context).unwrap();
    fs::write(context.join("payload"), "cache contents").unwrap();
    let mut stage = Stage::new(
        store.clone(),
        scratch(work.path()).unwrap(),
        "test".into(),
        &options(&context),
    )
    .unwrap();
    stage
        .copy("payload /payload", &BTreeMap::new(), &context, &[])
        .unwrap();
    let built = stage
        .finish(
            work.path(),
            BTreeMap::from([("VERSION".into(), "1".into())]),
        )
        .unwrap();
    (work, store, built)
}

#[test]
fn run_checkpoint_survives_reopen_and_corruption_is_a_miss() {
    let (work, store, built) = checkpoint_fixture();
    let key = digest(b"checkpoint");
    let cache = cache::Cache::open(&store).unwrap();
    let _lease = cache.lease(false).unwrap();
    let _key = cache.key_lock(&key).unwrap();
    cache.save(&key, &built).unwrap();
    let reopened = cache::Cache::open(&Store::open(store.root.clone()).unwrap()).unwrap();
    let loaded = reopened.load(&key, work.path()).unwrap().unwrap();
    assert_eq!(loaded.image.id, built.image.id);
    assert_eq!(loaded.args["VERSION"], "1");
    assert_eq!(
        fs::read(loaded.image.cache.temporary_root().unwrap().join("payload")).unwrap(),
        b"cache contents"
    );
    let blob = store
        .root
        .join("build-cache-v1/entries")
        .join(&key[7..])
        .join("blobs/sha256")
        .join(&built.image.id[7..]);
    fs::write(blob, "corrupt").unwrap();
    assert!(reopened.load(&key, work.path()).unwrap().is_none());
}

#[test]
fn copy_identity_tracks_only_selected_content_modes_and_links_not_mtime() {
    let work = tempfile::tempdir().unwrap();
    fs::write(work.path().join("selected"), "one").unwrap();
    fs::write(work.path().join("unused"), "irrelevant").unwrap();
    let sources = vec![PathBuf::from("selected")];
    let first = cache::fingerprint(work.path(), &sources).unwrap();
    fs::write(work.path().join("unused"), "changed").unwrap();
    File::open(work.path().join("selected"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
        .unwrap();
    assert_eq!(cache::fingerprint(work.path(), &sources).unwrap(), first);
    fs::write(work.path().join("selected"), "two").unwrap();
    let second = cache::fingerprint(work.path(), &sources).unwrap();
    assert_ne!(second, first);
    fs::set_permissions(
        work.path().join("selected"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert_ne!(cache::fingerprint(work.path(), &sources).unwrap(), second);
    symlink("selected", work.path().join("link")).unwrap();
    let link = vec![PathBuf::from("link")];
    let before = cache::fingerprint(work.path(), &link).unwrap();
    fs::remove_file(work.path().join("link")).unwrap();
    symlink("unused", work.path().join("link")).unwrap();
    assert_ne!(cache::fingerprint(work.path(), &link).unwrap(), before);
}

#[test]
fn copy_identity_cannot_follow_symlink_ancestors() {
    let work = tempfile::tempdir().unwrap();
    let context = work.path().join("context");
    fs::create_dir(&context).unwrap();
    symlink(work.path(), context.join("outside")).unwrap();
    fs::write(work.path().join("secret"), "private").unwrap();
    assert!(copy_sources("outside/secret /secret", &BTreeMap::new(), &context, &[]).is_err());
}

#[test]
fn cache_prune_filters_retention_and_publication_are_independent() {
    let (_work, store, built) = checkpoint_fixture();
    let key = digest(b"checkpoint");
    cache::Cache::open(&store)
        .unwrap()
        .save(&key, &built)
        .unwrap();
    assert_eq!(
        prune(
            &store,
            &PruneOptions {
                until: Some(Duration::from_secs(3600)),
                ..Default::default()
            }
        )
        .unwrap()
        .records,
        0
    );
    assert_eq!(
        prune(
            &store,
            &PruneOptions {
                keep_storage: Some(u64::MAX),
                ..Default::default()
            }
        )
        .unwrap()
        .records,
        0
    );
    let image = publish(&store, &built, &["cache:test".into()]).unwrap();
    assert_eq!(prune(&store, &PruneOptions::default()).unwrap().records, 0);
    let report = prune(
        &store,
        &PruneOptions {
            all: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(report.records, 1);
    assert!(report.bytes > 0);
    assert_eq!(
        prune(
            &store,
            &PruneOptions {
                all: true,
                ..Default::default()
            }
        )
        .unwrap()
        .records,
        0
    );
    let opened = store.open_image(&image).unwrap();
    pvisor_overlay_core::backend::materialize_tree(opened.rootfs()).unwrap();
    assert_eq!(
        fs::read(opened.rootfs().join("payload")).unwrap(),
        b"cache contents"
    );
}

#[test]
fn cache_lease_excludes_prune_until_build_exits() {
    let (_work, store, _) = checkpoint_fixture();
    let cache = cache::Cache::open(&store).unwrap();
    let lease = cache.lease(false).unwrap();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(store.root.join("build-cache-v1/lease"))
        .unwrap();
    assert!(file.try_lock_exclusive().is_err());
    drop(lease);
    file.try_lock_exclusive().unwrap();
}

#[test]
fn cache_prune_evicts_oldest_entries_and_orphaned_locks() {
    let (_work, store, built) = checkpoint_fixture();
    let cache = cache::Cache::open(&store).unwrap();
    let mut keys = Vec::new();
    for i in 1..=3 {
        let key = digest(format!("checkpoint-{i}").as_bytes());
        let lock = cache.key_lock(&key).unwrap();
        cache.save(&key, &built).unwrap();
        fs::write(
            store
                .root
                .join("build-cache-v1/entries")
                .join(&key[7..])
                .join("last-used"),
            format!("{i}"),
        )
        .unwrap();
        drop(lock);
        keys.push(key);
    }
    let first = store
        .root
        .join("build-cache-v1/entries")
        .join(&keys[0][7..]);
    let mut bytes = fs::metadata(first.join("record.json")).unwrap().len()
        + fs::metadata(first.join("last-used")).unwrap().len();
    for blob in fs::read_dir(first.join("blobs/sha256")).unwrap() {
        bytes += blob.unwrap().metadata().unwrap().len();
    }
    let report = prune(
        &store,
        &PruneOptions {
            all: true,
            keep_storage: Some(bytes * 2),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((report.records, report.bytes), (1, bytes));
    assert!(!first.exists());
    assert!(
        !store
            .root
            .join("build-cache-v1/locks")
            .join(&keys[0][7..])
            .exists()
    );
    for key in &keys[1..] {
        assert!(
            store
                .root
                .join("build-cache-v1/entries")
                .join(&key[7..])
                .exists()
        );
        assert!(
            store
                .root
                .join("build-cache-v1/locks")
                .join(&key[7..])
                .exists()
        );
    }
}

#[test]
fn sealed_filesystem_is_reused_for_metadata_only_publication() {
    let (work, store, built) = checkpoint_fixture();
    let root = built.image.cache.temporary_root().unwrap().to_owned();
    let original = built.image.id.clone();
    let mut stage = Stage::new(store, built, "next".into(), &options(work.path())).unwrap();
    stage.config["Labels"]["cache"] = "metadata".into();
    let finished = stage.finish(work.path(), BTreeMap::new()).unwrap();
    assert_ne!(finished.image.id, original);
    assert_eq!(finished.image.cache.temporary_root().unwrap(), root);
}

#[test]
fn stage_root_enables_virtual_metadata_without_opening_host_storage() {
    let work = tempfile::tempdir().unwrap();
    let context = work.path().join("context");
    fs::create_dir(&context).unwrap();
    let base = scratch(work.path()).unwrap();
    let metadata = fs::symlink_metadata(base.image.cache.temporary_root().unwrap()).unwrap();
    let expected =
        filesystem::identity(base.image.cache.temporary_root().unwrap(), &metadata).unwrap();
    let store = Store::open(work.path().join("store")).unwrap();
    let stage = Stage::new(store, base, "test".into(), &options(&context)).unwrap();
    let upper = stage.store.directory(&stage.instance.id).join("upper");
    let physical = fs::symlink_metadata(&upper).unwrap();
    assert_eq!(physical.mode() & 0o777, 0o700);
    assert_eq!(filesystem::identity(&upper, &physical).unwrap(), expected);
}

#[test]
fn scratch_multistage_build_exports_oci_and_publishes_local_launch_defaults() {
    let work = tempfile::tempdir().unwrap();
    let context = work.path().join("context");
    fs::create_dir(&context).unwrap();
    fs::write(context.join("payload"), b"binary\0\xffdata").unwrap();
    fs::write(context.join("Dockerfile"),"ARG BASE=scratch\nFROM $BASE AS source\nCOPY --chown=123:456 --chmod=0550 payload /app/file\nFROM scratch\nCOPY --from=source /app/ /result/\nARG MESSAGE=hello\nENV MESSAGE=$MESSAGE OTHER=world\nLABEL example.owner=team\nWORKDIR /result\nUSER 123:456\nEXPOSE 8080\nCMD [\"/result/file\",\"a b\",\"$literal\"]\n").unwrap();
    let store = Store::open(work.path().join("store")).unwrap();
    let mut opts = options(&context);
    opts.output = Some(work.path().join("image.tar"));
    let image = build(&store, &opts).unwrap();
    let opened = store.open_image(&image).unwrap();
    let rootfs = opened.rootfs();
    pvisor_overlay_core::backend::materialize_tree(rootfs).unwrap();
    assert_eq!(
        fs::read(rootfs.join("result/file")).unwrap(),
        b"binary\0\xffdata"
    );
    assert_eq!(image.user, "123:456");
    assert_eq!(image.cwd, Path::new("/result"));
    assert_eq!(image.cmd, ["/result/file", "a b", "$literal"]);
    assert_eq!(image.env["MESSAGE"], "hello");
    assert!(!image.env.contains_key("BASE"));
    assert_eq!(store.images().unwrap().len(), 1);
    let exported = tempfile::tempdir().unwrap();
    tar::Archive::new(File::open(opts.output.as_ref().unwrap()).unwrap())
        .unpack(exported.path())
        .unwrap();
    let layout = exported.path();
    let index: Value =
        serde_json::from_reader(File::open(layout.join("index.json")).unwrap()).unwrap();
    assert_eq!(index["manifests"][0]["digest"], image.digest);
    let config: Value = serde_json::from_reader(
        File::open(layout.join("blobs/sha256").join(&image.id[7..])).unwrap(),
    )
    .unwrap();
    assert_eq!(config["config"]["Labels"]["example.owner"], "team");
    let diff = config["rootfs"]["diff_ids"][0].as_str().unwrap();
    let layer = layout.join("blobs/sha256").join(&diff[7..]);
    assert_eq!(file_digest(&layer).unwrap(), diff);
    let mut found = false;
    for entry in tar::Archive::new(File::open(layer).unwrap())
        .entries()
        .unwrap()
    {
        let entry = entry.unwrap();
        if entry.path().unwrap() == Path::new("result/file") {
            found = true;
            assert_eq!(entry.header().uid().unwrap(), 0);
            assert_eq!(entry.header().mode().unwrap() & 0o7777, 0o550);
        }
    }
    assert!(found);
    let names = tar::Archive::new(File::open(opts.output.unwrap()).unwrap())
        .entries()
        .unwrap()
        .map(|e| e.unwrap().path().unwrap().into_owned())
        .collect::<Vec<_>>();
    assert!(names.contains(&PathBuf::from("oci-layout")));
    assert!(!names.iter().any(|p| p.starts_with("rootfs")));
    assert!(
        fs::read_dir(store.root.join("builds"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn failed_build_keeps_existing_tag_and_context_ignore_is_enforced() {
    let work = tempfile::tempdir().unwrap();
    let context = work.path().join("context");
    fs::create_dir(&context).unwrap();
    fs::write(context.join("file"), "retained").unwrap();
    fs::write(
        context.join("Dockerfile"),
        "FROM scratch\nCOPY file /file\n",
    )
    .unwrap();
    let store = Store::open(work.path().join("store")).unwrap();
    let opts = options(&context);
    let first = build(&store, &opts).unwrap();
    fs::write(context.join(".dockerignore"), "file\n").unwrap();
    assert!(
        build(&store, &opts)
            .unwrap_err()
            .to_string()
            .contains("COPY")
    );
    assert_eq!(store.images().unwrap()[0].id, first.id);
    assert!(
        fs::read_dir(store.root.join("builds"))
            .unwrap()
            .next()
            .is_none()
    );
    fs::write(context.join("Dockerfile"), "FROM scratch\nADD file /file\n").unwrap();
    assert!(build(&store, &opts).is_err());
    assert_eq!(store.images().unwrap()[0].id, first.id);
}

#[test]
fn context_capture_never_follows_external_symlinks_and_rejects_special_files() {
    let work = tempfile::tempdir().unwrap();
    let context = work.path().join("context");
    fs::create_dir(&context).unwrap();
    let secret = work.path().join("secret");
    fs::write(&secret, "host secret").unwrap();
    symlink(&secret, context.join("link")).unwrap();
    filesystem::snapshot(
        &context,
        &work.path().join("snapshot"),
        &filesystem::Ignore::new("").unwrap(),
    )
    .unwrap();
    assert!(
        fs::symlink_metadata(work.path().join("snapshot/link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_link(work.path().join("snapshot/link")).unwrap(),
        secret
    );
    let fifo = context.join("fifo");
    let path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(
        filesystem::snapshot(
            &context,
            &work.path().join("bad"),
            &filesystem::Ignore::new("").unwrap()
        )
        .is_err()
    );
}

#[test]
fn guest_absolute_symlinks_cannot_redirect_host_copy_outside_the_overlay() {
    let work = tempfile::tempdir().unwrap();
    let lower = work.path().join("lower");
    fs::create_dir(&lower).unwrap();
    let outside = work.path().join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, lower.join("escape")).unwrap();
    let core = OverlayCore::new(
        vec![lower],
        work.path().join("upper"),
        Some(work.path().join("work")),
    )
    .unwrap();
    let source = work.path().join("payload");
    fs::write(&source, "guest only").unwrap();
    filesystem::copy(
        &core,
        &source,
        Path::new("escape/file"),
        (0, 0),
        None,
        &mut BTreeMap::new(),
    )
    .unwrap();
    assert!(!outside.join("file").exists());
    let in_guest = outside.strip_prefix("/").unwrap().join("file");
    assert_eq!(
        fs::read(core.resolve_checked(&in_guest).unwrap().unwrap().path).unwrap(),
        b"guest only"
    );
}

#[test]
fn flatten_export_applies_whiteouts_and_preserves_hardlinks_and_guest_identity() {
    let work = tempfile::tempdir().unwrap();
    let lower = work.path().join("lower");
    fs::create_dir(&lower).unwrap();
    fs::write(lower.join("removed"), "old").unwrap();
    let core = OverlayCore::new(
        vec![lower],
        work.path().join("upper"),
        Some(work.path().join("work")),
    )
    .unwrap();
    core.remove(Path::new("removed"), false).unwrap();
    fs::write(core.upper_path(Path::new("file")), b"content").unwrap();
    filesystem::set_identity(&core.upper_path(Path::new("file")), 77, 88, 0o4750).unwrap();
    fs::hard_link(
        core.upper_path(Path::new("file")),
        core.upper_path(Path::new("alias")),
    )
    .unwrap();
    let layer = work.path().join("layer");
    filesystem::export(&core, &layer).unwrap();
    let mut count = 0;
    let mut hardlinks = 0;
    for entry in tar::Archive::new(File::open(&layer).unwrap())
        .entries()
        .unwrap()
    {
        let entry = entry.unwrap();
        let path = entry.path().unwrap();
        assert!(!path.to_string_lossy().contains(".wh."));
        assert_ne!(path, Path::new("removed"));
        if path != Path::new(".") {
            count += 1;
            assert_eq!(entry.header().uid().unwrap(), 77);
            assert_eq!(entry.header().gid().unwrap(), 88);
            assert_eq!(entry.header().mode().unwrap() & 0o7777, 0o4750);
            if entry.header().entry_type().is_hard_link() {
                hardlinks += 1;
            }
        }
    }
    assert_eq!(count, 2);
    assert_eq!(hardlinks, 1);
    let imported = work.path().join("imported");
    fs::create_dir(&imported).unwrap();
    import_layer(&layer, &imported).unwrap();
    assert_eq!(fs::read(imported.join("file")).unwrap(), b"content");
    assert!(!imported.join("removed").exists());
    assert_eq!(
        filesystem::identity(
            &imported.join("file"),
            &fs::symlink_metadata(imported.join("file")).unwrap()
        )
        .unwrap(),
        (77, 88, 0o104750)
    );
    assert_eq!(
        fs::metadata(imported.join("file")).unwrap().mode() & 0o6000,
        0
    );
}

#[test]
fn target_stops_before_unneeded_bases_and_invalid_requests_do_not_execute() {
    let work = tempfile::tempdir().unwrap();
    let context = work.path().join("context");
    fs::create_dir(&context).unwrap();
    fs::write(context.join("payload"), "kept").unwrap();
    fs::write(context.join("Dockerfile"), "FROM scratch AS selected\nCOPY payload /payload\nFROM do-not-pull.invalid/image\nRUN false\n").unwrap();
    let store = Store::open(work.path().join("store")).unwrap();
    let mut opts = options(&context);
    opts.target = Some("selected".into());
    assert!(build(&store, &opts).is_ok());
    opts.target = Some("0".into());
    assert!(build(&store, &opts).is_ok());
    opts.target = Some("missing".into());
    assert!(
        build(&store, &opts)
            .unwrap_err()
            .to_string()
            .contains("unknown target")
    );
    for tag in ["bad:", "a//b", "UPPER:tag", "/name", "name:-bad"] {
        assert!(validate_tag(tag).is_err(), "{tag}");
    }
    for tag in ["app", "app:Tag_1", "localhost:5000/team/app:v1"] {
        assert!(validate_tag(tag).is_ok(), "{tag}");
    }
}

#[test]
fn context_ignore_negation_and_specific_ignore_keep_only_selected_files() {
    let work = tempfile::tempdir().unwrap();
    let context = work.path().join("context");
    fs::create_dir_all(context.join("secret")).unwrap();
    fs::write(context.join("secret/keep"), "keep").unwrap();
    fs::write(context.join("secret/drop"), "drop").unwrap();
    fs::write(context.join(".dockerignore"), "**\n").unwrap();
    fs::write(
        context.join("Dockerfile.dockerignore"),
        "secret/*\n!secret/keep\n",
    )
    .unwrap();
    fs::write(
        context.join("Dockerfile"),
        "FROM scratch\nCOPY secret /data\n",
    )
    .unwrap();
    let store = Store::open(work.path().join("store")).unwrap();
    let mut opts = options(&context);
    opts.output = Some(work.path().join("export.tar"));
    let image = build(&store, &opts).unwrap();
    let opened = store.open_image(&image).unwrap();
    let rootfs = opened.rootfs();
    pvisor_overlay_core::backend::materialize_tree(rootfs).unwrap();
    assert!(rootfs.join("data/keep").is_file());
    assert!(!rootfs.join("data/drop").exists());
}

#[test]
fn copy_preserves_context_hardlinks_across_separately_selected_sources() {
    let work = tempfile::tempdir().unwrap();
    let context = work.path().join("context");
    fs::create_dir_all(context.join("sub")).unwrap();
    fs::write(context.join("first"), "shared bytes").unwrap();
    fs::hard_link(context.join("first"), context.join("sub/second")).unwrap();
    fs::write(
        context.join("Dockerfile"),
        "FROM scratch\nCOPY first sub/second /out/\n",
    )
    .unwrap();
    let store = Store::open(work.path().join("store")).unwrap();
    let mut opts = options(&context);
    opts.output = Some(work.path().join("export.tar"));
    let image = build(&store, &opts).unwrap();
    let opened = store.open_image(&image).unwrap();
    let rootfs = opened.rootfs();
    pvisor_overlay_core::backend::materialize_tree(rootfs).unwrap();
    let first = fs::metadata(rootfs.join("out/first")).unwrap();
    let second = fs::metadata(rootfs.join("out/second")).unwrap();
    let source = fs::metadata(context.join("first")).unwrap();
    assert_eq!((first.dev(), first.ino()), (second.dev(), second.ino()));
    assert_ne!((first.dev(), first.ino()), (source.dev(), source.ino()));
    assert_eq!(
        fs::read(rootfs.join("out/second")).unwrap(),
        b"shared bytes"
    );
}

#[test]
fn copied_symlink_exports_guest_root_without_mutating_host_target() {
    let work = tempfile::tempdir().unwrap();
    let context = work.path().join("context");
    fs::create_dir(&context).unwrap();
    let outside = work.path().join("outside");
    fs::write(&outside, "host bytes").unwrap();
    let before = fs::metadata(&outside).unwrap();
    symlink(&outside, context.join("link")).unwrap();
    let expected_mode = fs::symlink_metadata(context.join("link")).unwrap().mode();
    fs::write(
        context.join("Dockerfile"),
        "FROM scratch\nCOPY link /link\n",
    )
    .unwrap();
    let store = Store::open(work.path().join("store")).unwrap();
    let mut opts = options(&context);
    opts.output = Some(work.path().join("export.tar"));
    let image = build(&store, &opts).unwrap();
    let opened = store.open_image(&image).unwrap();
    let rootfs = opened.rootfs();
    pvisor_overlay_core::backend::materialize_tree(rootfs).unwrap();
    let link = rootfs.join("link");
    assert_eq!(fs::read_link(&link).unwrap(), outside);
    let metadata = fs::symlink_metadata(&link).unwrap();
    assert_eq!(
        filesystem::identity(&link, &metadata).unwrap(),
        (0, 0, expected_mode)
    );
    let after = fs::metadata(&outside).unwrap();
    assert_eq!(
        (before.uid(), before.gid(), before.mode()),
        (after.uid(), after.gid(), after.mode())
    );
    assert_eq!(fs::read(&outside).unwrap(), b"host bytes");
    let exported = tempfile::tempdir().unwrap();
    tar::Archive::new(File::open(opts.output.as_ref().unwrap()).unwrap())
        .unpack(exported.path())
        .unwrap();
    let layout = exported.path();
    let config: Value = serde_json::from_reader(
        File::open(layout.join("blobs/sha256").join(&image.id[7..])).unwrap(),
    )
    .unwrap();
    let digest = config["rootfs"]["diff_ids"][0].as_str().unwrap();
    let mut found = false;
    for entry in
        tar::Archive::new(File::open(layout.join("blobs/sha256").join(&digest[7..])).unwrap())
            .entries()
            .unwrap()
    {
        let entry = entry.unwrap();
        if entry.path().unwrap() == Path::new("link") {
            found = true;
            assert!(entry.header().entry_type().is_symlink());
            assert_eq!(
                (entry.header().uid().unwrap(), entry.header().gid().unwrap()),
                (0, 0)
            );
            assert_eq!(entry.link_name().unwrap().unwrap(), outside);
        }
    }
    assert!(found);
    #[cfg(target_os = "linux")]
    if unsafe { libc::geteuid() } != 0 {
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
        assert!(filesystem::set_identity(&link, 123, 456, 0o777).is_err());
        assert_eq!(filesystem::identity(&link, &metadata).unwrap().0, 0);
    }
}
