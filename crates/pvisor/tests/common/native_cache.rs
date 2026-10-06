//! Real native-cache image fixtures shared by lifecycle and CPU experiments.
use pvisor::cache::{CacheBackend, CacheClient, CacheConfig, Response};
use pvisor_core::node::EnvironmentLayer;
use sha2::{Digest, Sha256};
use std::{fs, path::Path, process::Command};
fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
pub(crate) fn publish_layer(
    source: &Path,
    cache: &Path,
    name: &str,
    files: &[(&str, &str)],
    base: bool,
) -> EnvironmentLayer {
    publish_layer_prepared(source, cache, name, files, base, |_| {})
}
pub(crate) fn copy_program(root: &Path, program: &str) {
    let copy = |path: &Path| {
        let target = root.join(path.strip_prefix("/").unwrap());
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(path, target).unwrap();
    };
    copy(Path::new(program));
    let output = Command::new("ldd").arg(program).output().unwrap();
    assert!(
        output.status.success(),
        "ldd {program}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for word in String::from_utf8(output.stdout).unwrap().split_whitespace() {
        if word.starts_with('/') {
            copy(Path::new(word));
        }
    }
}
pub(crate) fn publish_layer_prepared(
    source: &Path,
    cache: &Path,
    name: &str,
    files: &[(&str, &str)],
    base: bool,
    prepare: impl FnOnce(&Path),
) -> EnvironmentLayer {
    // Seed the native prepared-image fixture, then use the real cache publisher.
    // The generated small base contains actual host ELF programs/libraries.
    let manifest = format!("sha256:{}", sha(name.as_bytes()));
    let root = source.join("rootfs-v3/sha256").join(&manifest[7..]);
    fs::create_dir_all(&root).unwrap();
    if base {
        copy_program(&root, "/bin/sh");
        copy_program(&root, "/bin/sleep");
        for dir in ["tmp", "proc", "sys", "dev", "root", "etc"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
    }
    for (path, content) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    prepare(&root);
    let metadata = source.join("metadata/prepared-v1");
    fs::create_dir_all(&metadata).unwrap();
    let key = serde_json::to_vec(&[
        "registry-1.docker.io",
        &format!("library/{name}"),
        "test",
        "amd64",
    ])
    .unwrap();
    fs::write(
        metadata.join(format!("{}.json", sha(&key))),
        serde_json::to_vec(&serde_json::json!({
            "checked_at": pvisor_core::unix_now_ms()/1000,
            "prepared": {"digest": manifest, "env": {}, "entrypoint": [], "cmd": []}
        }))
        .unwrap(),
    )
    .unwrap();
    let client = CacheClient::from_config(CacheConfig {
        backend: CacheBackend::Filesystem,
        location: cache.display().to_string(),
        read_only: false,
        image_store: Some(source.to_owned()),
    })
    .unwrap();
    let Response::Prepared {
        image_handle: handle,
        digest,
        ..
    } = client
        .publish(&format!("{name}:test"), "amd64", false)
        .unwrap()
    else {
        panic!("missing native revision handle")
    };
    EnvironmentLayer {
        handle,
        manifest_digest: digest,
    }
}
