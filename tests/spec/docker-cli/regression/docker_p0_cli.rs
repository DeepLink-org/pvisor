//! Docker P0 contracts through installed commands and public runtime entrypoints.
use pvisor::docker::{Create, Store};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn call(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_DOCKER_HOME", home)
        .env_remove("PVISOR_IMAGE_STORE")
        .args(args)
        .output()
        .unwrap()
}
fn success(home: &Path, args: &[&str]) -> Output {
    let output = call(home, args);
    assert!(output.status.success(), "{args:?}: {output:?}");
    output
}
fn fixture() -> (tempfile::TempDir, Store, Create) {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("store");
    let context = directory.path().join("context");
    fs::create_dir(&context).unwrap();
    fs::write(context.join("lower"), b"image bytes\0\xff").unwrap();
    fs::write(context.join("Dockerfile"),
        "FROM scratch\nCOPY lower /lower\nLABEL inherited=base overridden=image\nCMD [\"/lower\"]\n").unwrap();
    success(
        &home,
        &["build", "-t", "fixture:test", context.to_str().unwrap()],
    );
    let store = Store::open(home).unwrap();
    let request = serde_json::from_value(json!({
        "name":"fixture", "image":"fixture:test", "command":[], "entrypoint":null,
        "env":{}, "cwd":null,"user":null,"mounts":[],"ports":[],"cpus":1,"memory_mib":32,
        "auto_remove":false,"interactive":false,"tty":null,"network":false,
        "labels":{"overridden":"container","role":"worker"}
    }))
    .unwrap();
    (directory, store, request)
}

#[test]
fn p0_help_and_invalid_arguments_do_not_create_storage() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("store");
    for args in [
        vec!["container", "inspect", "--help"],
        vec!["image", "inspect", "--help"],
        vec!["volume", "--help"],
        vec!["volume", "create", "--help"],
        vec!["volume", "inspect", "--help"],
        vec!["restart", "--help"],
        vec!["wait", "--help"],
        vec!["cp", "--help"],
    ] {
        success(&home, &args);
    }
    for args in [
        vec!["ps", "--filter", "unknown=x"],
        vec!["images", "--filter", "unknown=x"],
        vec!["ps", "--format", "{{printf .}}"],
        vec!["image", "inspect", "--format", "{{", "missing"],
        vec!["run", "--restart", "invalid", "unused"],
        vec!["run", "--restart", "always", "--rm", "unused"],
        vec!["run", "--stop-signal", "INVALID", "unused"],
        vec!["run", "--stop-timeout", "86401", "unused"],
        vec!["run", "--label", "=invalid", "unused"],
        vec!["cp", "one:/a", "two:/b"],
        vec!["cp", "local", "other"],
        vec!["cp", "-", "one:/a"],
    ] {
        assert!(!call(&home, &args).status.success(), "{args:?}");
    }
    assert!(!home.exists());
}

#[test]
fn inspect_labels_filters_formats_and_grouped_commands_use_effective_records() {
    let (directory, store, request) = fixture();
    let home = directory.path().join("store");
    let instance = store.create(request).unwrap();
    let output = success(&home, &["container", "inspect", "fixture"]);
    let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(metadata[0]["Id"], instance.id);
    assert_eq!(metadata[0]["Config"]["Labels"]["inherited"], "base");
    assert_eq!(metadata[0]["Config"]["Labels"]["overridden"], "container");
    assert!(metadata[0].get("cache").is_none());
    assert_eq!(
        success(
            &home,
            &[
                "container",
                "inspect",
                "-f",
                "{{index .Config.Labels \"role\"}}",
                "fixture"
            ]
        )
        .stdout,
        b"worker\n"
    );
    assert_eq!(
        success(
            &home,
            &["ps", "-aq", "--no-trunc", "-f", "label=role=worker"]
        )
        .stdout,
        format!("{}\n", instance.id).as_bytes()
    );
    assert!(
        success(&home, &["ps", "-aq", "-f", "label=missing"])
            .stdout
            .is_empty()
    );
    assert_eq!(
        success(
            &home,
            &["container", "ls", "-a", "--format", "{{.Names}}:{{.State}}"]
        )
        .stdout,
        b"fixture:created\n"
    );
    let image: Value =
        serde_json::from_slice(&success(&home, &["image", "inspect", "fixture:test"]).stdout)
            .unwrap();
    assert_eq!(image[0]["Config"]["Labels"]["overridden"], "image");
    assert_eq!(
        success(
            &home,
            &[
                "images",
                "--filter",
                "label=inherited=base",
                "--format",
                "{{.Repository}}:{{.Tag}}"
            ]
        )
        .stdout,
        b"fixture:test\n"
    );
    assert!(
        success(&home, &["images", "--filter", "reference=missing*", "-q"])
            .stdout
            .is_empty()
    );
    assert!(
        success(&home, &["images", "--filter", "dangling=true", "-q"])
            .stdout
            .is_empty()
    );
    assert!(
        !call(&home, &["container", "inspect", "missing"])
            .status
            .success()
    );
    assert!(call(&home, &["inspect", "--help"]).status.success());
}

#[test]
fn volume_commands_refuse_stopped_references_and_never_adopt_links() {
    let (directory, store, mut request) = fixture();
    let home = directory.path().join("store");
    assert_eq!(
        success(&home, &["volume", "create", "data"]).stdout,
        b"data\n"
    );
    success(&home, &["volume", "create", "data"]);
    fs::write(store.volume("data").unwrap().join("keep"), b"persistent").unwrap();
    request.mounts.push(pvisor::docker::Mount {
        source: store.volume("data").unwrap(),
        target: "/data".into(),
        read_only: false,
        staged: false,
        volume: Some("data".into()),
    });
    store.create(request).unwrap();
    assert!(!call(&home, &["volume", "rm", "data"]).status.success());
    assert_eq!(
        fs::read(home.join("volumes/data/keep")).unwrap(),
        b"persistent"
    );
    success(&home, &["rm", "-v", "fixture"]);
    assert_eq!(success(&home, &["volume", "ls", "-q"]).stdout, b"data\n");
    assert_eq!(
        success(&home, &["volume", "inspect", "-f", "{{.Driver}}", "data"]).stdout,
        b"local\n"
    );
    success(&home, &["volume", "rm", "data"]);
    assert!(!home.join("volumes/data").exists());
    assert!(
        !call(&home, &["volume", "inspect", "missing"])
            .status
            .success()
    );
    assert!(!home.join("volumes/missing").exists());
    std::os::unix::fs::symlink(directory.path(), home.join("volumes/unsafe")).unwrap();
    assert!(
        !call(&home, &["volume", "create", "unsafe"])
            .status
            .success()
    );
    assert!(!call(&home, &["volume", "rm", "unsafe"]).status.success());
}

#[test]
fn stopped_copy_uses_merged_content_and_refuses_host_links_and_traversal() {
    let (directory, store, request) = fixture();
    let home = directory.path().join("store");
    store.create(request).unwrap();
    let result = directory.path().join("result");
    success(&home, &["cp", "fixture:/lower", result.to_str().unwrap()]);
    assert_eq!(fs::read(&result).unwrap(), b"image bytes\0\xff");
    let bytes: Vec<_> = (0..=255).cycle().take(1024 * 1024 + 17).collect();
    let input = directory.path().join("input");
    fs::write(&input, &bytes).unwrap();
    success(&home, &["cp", input.to_str().unwrap(), "fixture:/uploaded"]);
    success(
        &home,
        &["cp", "fixture:/uploaded", result.to_str().unwrap()],
    );
    assert_eq!(fs::read(&result).unwrap(), bytes);
    let tree = directory.path().join("tree");
    fs::create_dir_all(tree.join("nested")).unwrap();
    fs::write(tree.join("nested/file"), b"nested").unwrap();
    success(
        &home,
        &["container", "cp", tree.to_str().unwrap(), "fixture:/tree"],
    );
    let out = directory.path().join("out");
    fs::create_dir(&out).unwrap();
    success(&home, &["cp", "fixture:/tree/.", out.to_str().unwrap()]);
    assert_eq!(fs::read(out.join("nested/file")).unwrap(), b"nested");
    assert!(!out.join("tree").exists());
    let secret = directory.path().join("secret");
    fs::write(&secret, b"untouched").unwrap();
    let link = directory.path().join("link");
    std::os::unix::fs::symlink(&secret, &link).unwrap();
    assert!(
        !call(&home, &["cp", "fixture:/lower", link.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(fs::read(secret).unwrap(), b"untouched");
    assert!(
        !call(
            &home,
            &["cp", "fixture:/../lower", result.to_str().unwrap()]
        )
        .status
        .success()
    );
    assert_eq!(success(&home, &["wait", "fixture"]).stdout, b"0\n");
}

#[test]
fn failed_copy_preserves_existing_destinations_and_other_container_content() {
    let (directory, store, request) = fixture();
    let mut other = request.clone();
    other.name = Some("other".into());
    store.create(request).unwrap();
    store.create(other).unwrap();
    let home = directory.path().join("store");
    let source = directory.path().join("source");
    let result = directory.path().join("result");
    let bytes = b"owned destination\0\xff";
    fs::write(&source, bytes).unwrap();
    success(&home, &["cp", source.to_str().unwrap(), "fixture:/saved"]);
    fs::write(&result, bytes).unwrap();
    for args in [
        vec!["cp", "fixture:/absent", result.to_str().unwrap()],
        vec!["cp", "missing:/lower", result.to_str().unwrap()],
        vec!["cp", "fixture:/../lower", result.to_str().unwrap()],
    ] {
        let failed = call(&home, &args);
        assert!(!failed.status.success(), "{args:?}: {failed:?}");
        assert_eq!(fs::read(&result).unwrap(), bytes);
    }
    let absent = directory.path().join("absent");
    let failed = call(&home, &["cp", absent.to_str().unwrap(), "fixture:/saved"]);
    assert!(!failed.status.success(), "{failed:?}");
    success(&home, &["cp", "fixture:/saved", result.to_str().unwrap()]);
    assert_eq!(fs::read(&result).unwrap(), bytes);
    success(&home, &["cp", "other:/lower", result.to_str().unwrap()]);
    assert_eq!(fs::read(&result).unwrap(), b"image bytes\0\xff");
    assert!(
        !call(&home, &["cp", "other:/saved", result.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(fs::read(&result).unwrap(), b"image bytes\0\xff");
    assert_eq!(store.instances(true).unwrap().len(), 2);
}

#[test]
fn compose_lifecycle_fields_validate_without_opening_storage() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("store");
    let file = directory.path().join("compose.yaml");
    fs::write(
        &file,
        r#"
name: lifecycle
services:
  web:
    image: alpine:3.20
    restart: on-failure:3
    stop_signal: SIGUSR1
    stop_grace_period: 1m500ms
    labels: ["role=web", "empty"]
"#,
    )
    .unwrap();
    let output = success(&home, &["compose", "-f", file.to_str().unwrap(), "config"]);
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("stop_grace_period: 1m500ms")
    );
    for field in [
        "restart: invalid",
        "stop_signal: INVALID",
        "stop_grace_period: -1s",
        "labels: {com.docker.compose.project: spoofed}",
    ] {
        fs::write(
            &file,
            format!("services:\n  web:\n    image: alpine\n    {field}\n"),
        )
        .unwrap();
        assert!(
            !call(
                &home,
                &["compose", "-f", file.to_str().unwrap(), "config", "-q"]
            )
            .status
            .success(),
            "{field}"
        );
    }
    assert!(!home.exists());
}

#[test]
fn container_is_a_durable_session_not_a_parallel_instance_record() {
    let (directory, store, request) = fixture();
    let instance = store.create(request).unwrap();
    let session = store.session(&instance.id).unwrap();
    assert_eq!(session.run_id(), instance.id);
    assert_eq!(session.kind(), pvisor::SessionKind::Container);
    assert_eq!(session.state(), pvisor::SessionState::Created);
    assert!(session.active_execution().is_empty());
    assert!(session.root().join("session.json").is_file());
    assert!(!session.root().join("instance.json").exists());
    let decoded: pvisor::Session<pvisor::docker::ContainerProfile> =
        serde_json::from_reader(fs::File::open(session.root().join("session.json")).unwrap())
            .unwrap();
    assert_eq!(decoded.run_id(), session.run_id());
    let home = directory.path().join("store");
    assert_eq!(
        success(
            &home,
            &["inspect", "-f", "{{.Pvisor.SessionId}}", "fixture"]
        )
        .stdout,
        format!("{}\n", instance.id).as_bytes()
    );
}

#[test]
fn ancestor_aliases_resolve_to_the_pinned_image_identity() {
    let (directory, store, mut request) = fixture();
    let image = store.images().unwrap().remove(0);
    request.image = image.id.clone();
    let instance = store.create(request).unwrap();
    let home = directory.path().join("store");
    for selector in [
        &image.id,
        "fixture:test",
        "docker.io/library/fixture:test",
        "registry-1.docker.io/library/fixture:test",
    ] {
        assert_eq!(
            success(
                &home,
                &[
                    "ps",
                    "-aq",
                    "--no-trunc",
                    "-f",
                    &format!("ancestor={selector}")
                ]
            )
            .stdout,
            format!("{}\n", instance.id).as_bytes(),
            "{selector}"
        );
    }
    assert!(
        success(&home, &["ps", "-aq", "-f", "ancestor=missing:test"])
            .stdout
            .is_empty()
    );
}

#[test]
fn copy_retains_directory_syntax_and_explicit_host_parent_paths() {
    let (directory, store, request) = fixture();
    store.create(request).unwrap();
    let home = directory.path().join("store");
    let missing = directory.path().join("missing");
    let output = call(
        &home,
        &["cp", "fixture:/lower", &format!("{}/", missing.display())],
    );
    assert!(!output.status.success(), "{output:?}");
    assert!(!missing.exists());
    let source = directory.path().join("source");
    fs::write(&source, b"parent-path").unwrap();
    fs::create_dir(directory.path().join("child")).unwrap();
    let parent = directory.path().join("child/../source");
    success(&home, &["cp", parent.to_str().unwrap(), "fixture:/parent"]);
    let result = directory.path().join("result");
    success(&home, &["cp", "fixture:/parent", result.to_str().unwrap()]);
    assert_eq!(fs::read(result).unwrap(), b"parent-path");
    assert!(
        !call(&home, &["cp", source.to_str().unwrap(), "fixture:/absent/"])
            .status
            .success()
    );
}

#[test]
fn a_literal_bash_docker_alias_routes_metadata_export_and_unknown_commands_safely() {
    let (directory, store, request) = fixture();
    store.create(request).unwrap();
    let binary = Path::new(env!("CARGO_BIN_EXE_pvisor"));
    let path = std::env::join_paths(
        std::iter::once(binary.parent().unwrap().to_path_buf()).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )),
    )
    .unwrap();
    let script = r#"
set -eu
shopt -s expand_aliases
alias docker=pvisor
docker inspect -f '{{.Pvisor.Kind}}' fixture
docker inspect --type image -f '{{.Id}}' fixture:test
docker ps -a --format '{{.Names}}'
docker export -o rootfs.tar fixture
if docker touch must-not-exist; then exit 90; fi
test ! -e must-not-exist
"#;
    let output = Command::new("bash")
        .args(["--noprofile", "--norc", "-c", script])
        .current_dir(directory.path())
        .env("PATH", path)
        .env("PVISOR_DOCKER_HOME", directory.path().join("store"))
        .env_remove("PVISOR_IMAGE_STORE")
        .env("PVISOR_RUN_HOME", directory.path().join("tasks"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .starts_with("container\nsha256:")
    );
    let mut archive =
        tar::Archive::new(fs::File::open(directory.path().join("rootfs.tar")).unwrap());
    let mut found = false;
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap() == Path::new("lower") {
            use std::io::Read;
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"image bytes\0\xff");
            found = true;
        }
    }
    assert!(found);
    assert!(
        !directory.path().join("tasks").exists(),
        "unknown Docker command launched a task"
    );
}
