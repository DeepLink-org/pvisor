//! Compose parsing and read-only configuration through the installed frontend.
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn call(directory: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .current_dir(directory)
        .env_remove("COMPOSE_PROJECT_NAME")
        .env_remove("PVISOR_COMPOSE_TEST_TAG")
        .env("PVISOR_DOCKER_HOME", directory.join("store"))
        .env("PVISOR_RUN_HOME", directory.join("sessions"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn compose_help_and_config_do_not_create_a_store_or_session() {
    let directory = tempfile::tempdir().unwrap();
    for args in [
        vec!["compose", "--help"],
        vec!["compose", "up", "--help"],
        vec!["compose", "exec", "--help"],
        vec!["compose", "logs", "--help"],
        vec!["compose", "ps", "--help"],
        vec!["compose", "down", "--help"],
        vec!["compose", "build", "--help"],
        vec!["help", "compose"],
    ] {
        let output = call(directory.path(), &args);
        assert!(output.status.success(), "{output:?}");
    }
    fs::write(directory.path().join("compose.yaml"),
        "name: demo\nservices:\n  web:\n    image: alpine:${PVISOR_COMPOSE_TEST_TAG:-3.20}\n    depends_on: [db]\n  db:\n    image: redis:7\n").unwrap();
    let config = call(directory.path(), &["compose", "-p", "override", "config"]);
    assert!(config.status.success(), "{config:?}");
    let text = String::from_utf8(config.stdout).unwrap();
    assert!(text.contains("name: override"), "{text}");
    assert!(text.contains("alpine:3.20"), "{text}");
    let services = call(directory.path(), &["compose", "config", "--services"]);
    assert_eq!(services.stdout, b"db\nweb\n");
    let quiet = call(directory.path(), &["compose", "config", "-q"]);
    assert!(quiet.status.success());
    assert!(quiet.stdout.is_empty());
    assert!(!directory.path().join("store").exists());
    assert!(!directory.path().join("sessions").exists());
}

#[test]
fn compose_invalid_config_options_and_selection_fail_before_storage() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("compose.yaml"),
        "services: {app: {image: alpine}}\n",
    )
    .unwrap();
    for args in [
        vec!["compose", "up"],
        vec!["compose", "up", "-d", "--build", "--no-build"],
        vec!["compose", "up", "-d", "--force-recreate", "--no-recreate"],
        vec!["compose", "up", "--wait", "--wait-timeout", "0"],
        vec!["compose", "up", "-d", "missing"],
        vec!["compose", "pull", "missing"],
        vec!["compose", "stop", "missing"],
        vec!["compose", "ps", "missing"],
        vec!["compose", "exec", "-T", "missing", "true"],
        vec![
            "compose",
            "-f",
            "compose.yaml",
            "-f",
            "other.yaml",
            "config",
        ],
        vec!["compose", "down", "--unsupported"],
    ] {
        let output = call(directory.path(), &args);
        assert!(!output.status.success(), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
    }
    fs::write(
        directory.path().join("compose.yaml"),
        "services: {app: {image: alpine, privileged: true}}\n",
    )
    .unwrap();
    let output = call(directory.path(), &["compose", "up", "-d"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("privileged"));
    assert!(!directory.path().join("store").exists());
    assert!(!directory.path().join("sessions").exists());
}

#[test]
fn compose_discovers_parent_files_and_empty_queries_are_project_scoped() {
    let directory = tempfile::tempdir().unwrap();
    let child = directory.path().join("child");
    fs::create_dir(&child).unwrap();
    fs::write(
        directory.path().join("docker-compose.yml"),
        "name: demo\nservices: {app: {image: alpine}}\n",
    )
    .unwrap();
    assert!(call(&child, &["compose", "config", "-q"]).status.success());
    assert!(!child.join("store").exists());
    for args in [
        vec!["compose", "ps", "-aq"],
        vec!["compose", "logs"],
        vec!["compose", "stop"],
        vec!["compose", "down"],
    ] {
        let output = call(&child, &args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
    }
}

#[test]
fn compose_build_uses_local_context_without_creating_service_containers() {
    let directory = tempfile::tempdir().unwrap();
    let context = directory.path().join("src");
    fs::create_dir(&context).unwrap();
    fs::write(
        context.join("Appfile"),
        "FROM scratch\nARG MESSAGE=default\nENV MESSAGE=$MESSAGE\nCMD [\"/bin/true\"]\n",
    )
    .unwrap();
    fs::write(
        directory.path().join("compose.yaml"),
        r#"
name: build-test
services:
  app:
    build:
      context: ./src
      dockerfile: Appfile
      args: {MESSAGE: from-compose}
volumes:
  data: {}
"#,
    )
    .unwrap();
    let config = call(directory.path(), &["compose", "config", "-q"]);
    assert!(config.status.success(), "{config:?}");
    assert!(!directory.path().join("store").exists());
    let no_build = call(directory.path(), &["compose", "up", "-d", "--no-build"]);
    assert!(!no_build.status.success(), "{no_build:?}");
    assert!(String::from_utf8_lossy(&no_build.stderr).contains("no local image"));
    let output = call(directory.path(), &["compose", "build", "--no-cache"]);
    assert!(output.status.success(), "{output:?}");
    let store = pvisor::docker::Store::open(directory.path().join("store")).unwrap();
    let images = store.images().unwrap();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].reference, "build-test-app:latest");
    assert_eq!(images[0].env["MESSAGE"], "from-compose");
    assert!(store.instances(true).unwrap().is_empty());
    assert_eq!(store.networks().unwrap().len(), 2);
    assert!(
        !directory
            .path()
            .join("store/volumes/build-test_data")
            .exists()
    );
    fs::write(context.join("Appfile"), "INVALID instruction\n").unwrap();
    let failed = call(directory.path(), &["compose", "build"]);
    assert!(!failed.status.success(), "{failed:?}");
    assert_eq!(store.images().unwrap()[0].id, images[0].id);
}

#[test]
fn compose_config_export_can_be_reloaded_without_reinterpreting_literal_dollars() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("compose.yaml"),
        r#"
name: roundtrip
services:
  app:
    image: alpine
    command: [sh, -c, "echo $$HOME; echo price=$5; echo trailing$"]
    environment:
      LITERAL: "$${HOME}"
    volumes: ["./data-$$HOME:/data:ro"]
"#,
    )
    .unwrap();
    let first = call(directory.path(), &["compose", "config"]);
    assert!(first.status.success(), "{first:?}");
    let output = String::from_utf8(first.stdout).unwrap();
    assert!(output.contains("echo $$HOME"), "{output}");
    fs::write(directory.path().join("resolved.yaml"), &output).unwrap();
    for _ in 0..3 {
        let reloaded = call(
            directory.path(),
            &["compose", "-f", "resolved.yaml", "config"],
        );
        assert!(reloaded.status.success(), "{reloaded:?}");
        assert_eq!(String::from_utf8(reloaded.stdout).unwrap(), output);
    }
    assert!(!directory.path().join("store").exists());
    assert!(!directory.path().join("sessions").exists());
}

#[test]
fn compose_yaml_merges_preserve_precedence_nulls_and_config_roundtrip() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("compose.yaml"),
        r#"
name: fragments
x-base: &base
  image: alpine:${PVISOR_COMPOSE_TEST_TAG:-3.20}
  entrypoint: [sh, -c, 'printf "%s" "$$1"', --]
  command: [inherited]
  environment: &environment
    KEEP: "$${LITERAL}"
    OVERRIDE: base
x-later: &later
  image: wrong:latest
  user: "123"
services:
  app:
    <<: [*base, *later]
    command: []
    environment:
      <<: *environment
      OVERRIDE: explicit
  reset:
    <<: *base
    entrypoint: null
    command: ''
"#,
    )
    .unwrap();
    let first = call(directory.path(), &["compose", "config"]);
    assert!(first.status.success(), "{first:?}");
    let config: serde_yaml::Value = serde_yaml::from_slice(&first.stdout).unwrap();
    let app = &config["services"]["app"];
    assert_eq!(app["image"].as_str(), Some("alpine:3.20"));
    assert_eq!(app["user"].as_str(), Some("123"));
    assert_eq!(app["entrypoint"].as_sequence().unwrap().len(), 4);
    assert!(app["command"].as_sequence().unwrap().is_empty());
    assert_eq!(app["environment"]["KEEP"].as_str(), Some("$${LITERAL}"));
    assert_eq!(app["environment"]["OVERRIDE"].as_str(), Some("explicit"));
    assert!(config["services"]["reset"]["entrypoint"].is_null());
    assert!(
        config["services"]["reset"]["command"]
            .as_sequence()
            .unwrap()
            .is_empty()
    );
    fs::write(directory.path().join("resolved.yaml"), &first.stdout).unwrap();
    let second = call(
        directory.path(),
        &["compose", "-f", "resolved.yaml", "config"],
    );
    assert!(second.status.success(), "{second:?}");
    assert_eq!(second.stdout, first.stdout);
    for yaml in [
        "services: {app: {<<: 42, image: alpine}}",
        "services: {app: {<<: [42], image: alpine}}",
        "x-base: &base {image: alpine, privileged: true}\nservices: {app: {<<: *base}}",
    ] {
        fs::write(directory.path().join("compose.yaml"), yaml).unwrap();
        let invalid = call(directory.path(), &["compose", "config", "-q"]);
        assert!(!invalid.status.success(), "{yaml}: {invalid:?}");
    }
    assert!(!directory.path().join("store").exists());
    assert!(!directory.path().join("sessions").exists());
}
