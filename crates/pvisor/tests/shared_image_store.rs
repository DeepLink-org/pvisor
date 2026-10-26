//! Exercise the public container builder and Job image preparation against one store.
use pvisor::docker::{
    Store,
    build::{BuildOptions, build},
};
use pvisor_overlay_core::backend;
use std::{collections::BTreeMap, fs, path::Path, process::Command, time::Duration};

#[test]
fn build_tags_and_job_images_share_one_local_v2_store() {
    const CHILD: &str = "PVISOR_SHARED_IMAGE_STORE_TEST_CHILD";
    if let Ok(root) = std::env::var(CHILD) {
        exercise(Path::new(&root));
        return;
    }
    for explicit_image_store in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "build_tags_and_job_images_share_one_local_v2_store",
                "--nocapture",
            ])
            .env(CHILD, &root)
            .env("PVISOR_DOCKER_HOME", root.join("docker"))
            .env("PVISOR_CACHE_SERVER", "off")
            .env_remove("PVISOR_CACHE_BACKEND")
            .env_remove("PVISOR_CACHE_LOCATION")
            .env_remove("PVISOR_CACHE_READ_ONLY");
        if explicit_image_store {
            command.env("PVISOR_IMAGE_STORE", root.join("shared-images"));
        } else {
            command.env_remove("PVISOR_IMAGE_STORE");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn exercise(root: &Path) {
    let image_root = std::env::var_os("PVISOR_IMAGE_STORE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("docker/images"));
    let store = Store::discover().unwrap();
    let context = root.join("context");
    fs::create_dir(&context).unwrap();
    fs::write(context.join("payload"), b"first generation").unwrap();
    fs::write(
        context.join("Dockerfile"),
        "FROM scratch\nCOPY payload /payload\nENV VERSION=first\nCMD [\"/payload\"]\n",
    )
    .unwrap();
    let options = BuildOptions {
        context: context.clone(),
        file: context.join("Dockerfile"),
        tags: vec![
            "shared:test".into(),
            "shared:alias".into(),
            "docker.io/library/shared:test".into(),
        ],
        build_args: BTreeMap::new(),
        target: None,
        platform: None,
        pull: false,
        cpus: 1,
        memory_mib: 32,
        network: false,
        timeout: Duration::from_secs(30),
        quiet: true,
        output: None,
    };
    let first = build(&store, &options).unwrap();
    assert_eq!(
        store.images().unwrap().len(),
        2,
        "canonical spellings share one catalog entry"
    );
    assert_eq!(first.cache.location, image_root.join("pool-v2"));
    // Local build aliases remain usable offline after the remote tag TTL.
    let pool_platform = first.cache.handle.split(':').nth(2).unwrap();
    for entry in fs::read_dir(image_root.join("pool-v2/meta")).unwrap() {
        let head = entry
            .unwrap()
            .path()
            .join("platforms")
            .join(pool_platform)
            .join("HEAD.json");
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&head).unwrap()).unwrap();
        record["published_at"] = 0.into();
        fs::write(head, serde_json::to_vec(&record).unwrap()).unwrap();
    }
    let (prepared, owner) = pvisor::cache::prepare_vm_image("shared:alias", None).unwrap();
    let owner = owner.expect("daemonless Job must use the shared lazy pool");
    assert_eq!(prepared.digest, first.digest);
    assert_eq!(prepared.env["VERSION"], "first");
    assert_eq!(prepared.cmd, vec!["/payload"]);
    assert!(prepared.rootfs.starts_with(image_root.join("projections")));
    assert_eq!(
        backend::read_at(&prepared.rootfs.join("payload"), 0, 64)
            .unwrap()
            .unwrap(),
        b"first generation"
    );
    let first_projection = prepared.rootfs.clone();

    fs::write(context.join("payload"), b"second generation").unwrap();
    fs::write(
        context.join("Dockerfile"),
        "FROM scratch\nCOPY payload /payload\nENV VERSION=second\nCMD [\"/payload\"]\n",
    )
    .unwrap();
    let mut replacement = options.clone();
    replacement.tags = vec!["shared:test".into()];
    let second = build(&store, &replacement).unwrap();
    let (updated, updated_owner) =
        pvisor::cache::prepare_vm_image("docker.io/library/shared:test", Some(image_root.clone()))
            .unwrap();
    assert_ne!(first.digest, second.digest);
    assert_eq!(updated.digest, second.digest);
    assert_eq!(updated.env["VERSION"], "second");
    assert_eq!(
        backend::read_at(&updated.rootfs.join("payload"), 0, 64)
            .unwrap()
            .unwrap(),
        b"second generation"
    );
    fs::write(
        context.join("Dockerfile"),
        "FROM index.docker.io/library/shared:test\n",
    )
    .unwrap();
    replacement.tags = vec!["derived:test".into()];
    let derived = build(&store, &replacement).unwrap();
    assert_eq!(derived.env["VERSION"], "second");
    assert_eq!(store.images().unwrap().len(), 3);
    store
        .rmi("registry-1.docker.io/library/shared:test", true)
        .unwrap();
    store.rmi("shared:alias", true).unwrap();
    let retained = store.open_image(&first).unwrap();
    assert_eq!(
        backend::read_at(&retained.rootfs().join("payload"), 0, 64)
            .unwrap()
            .unwrap(),
        b"first generation"
    );
    assert_eq!(
        backend::read_at(&first_projection.join("payload"), 0, 64)
            .unwrap()
            .unwrap(),
        b"first generation"
    );
    assert_eq!(fs::read_dir(image_root.join("staging")).unwrap().count(), 0);
    assert!(!image_root.join("rootfs-v3").exists());
    assert!(!image_root.join("blobs").exists());
    if image_root != root.join("docker/images") {
        assert!(!root.join("docker/images/pool-v2").exists());
    }

    // Published records have one V2 shape; old directory records are rejected.
    let record = serde_json::to_value(&first).unwrap();
    assert!(record.get("rootfs").is_none());
    for missing in ["cache", "platform"] {
        let mut incomplete = record.clone();
        incomplete.as_object_mut().unwrap().remove(missing);
        assert!(serde_json::from_value::<pvisor::docker::Image>(incomplete).is_err());
    }
    let mut legacy = record;
    legacy["rootfs"] = context.to_str().unwrap().into();
    assert!(serde_json::from_value::<pvisor::docker::Image>(legacy.clone()).is_err());
    let catalog_path = root.join("docker/images.json");
    let catalog = fs::read(&catalog_path).unwrap();
    fs::write(&catalog_path, serde_json::to_vec(&vec![legacy]).unwrap()).unwrap();
    assert!(store.images().is_err());
    fs::write(catalog_path, catalog).unwrap();

    // Corruption is an error, rather than a reason to resolve a mutable registry tag.
    fs::write(image_root.join("pool-v2/format.json"), b"corrupt").unwrap();
    assert!(pvisor::cache::prepare_vm_image("shared:alias", None).is_err());
    drop((owner, updated_owner, retained));
    assert!(!first_projection.exists());
    assert_eq!(
        fs::read_dir(image_root.join("projections"))
            .unwrap()
            .count(),
        0
    );
}
