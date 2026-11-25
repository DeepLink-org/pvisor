//! No-VM convergence gates. Native start/exec acceptance remains in docker_cli.
use super::tests::{load, store};
use super::*;

fn reconcile(project: &Project, store: &Store) -> Vec<Instance> {
    project
        .reconcile(store, &[], &UpOptions::default(), &mut |_| {})
        .unwrap()
}

#[test]
fn project_only_down_retries_after_a_failed_stop_request_under_concurrent_invocations() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let project = load(
        directory.path(),
        r#"
services:
  first:
    image: fixture:latest
    volumes: [data:/state]
  blocked:
    image: fixture:latest
volumes:
  data: {}
"#,
    )
    .unwrap();
    let members = reconcile(&project, &store);
    let member = |service: &str| {
        members
            .iter()
            .find(|entry| entry.compose.as_ref().unwrap().service == service)
            .unwrap()
            .clone()
    };
    let first = member("first");
    let mut blocked = member("blocked");
    blocked.created_ms = first.created_ms + 1;
    blocked.generation = uuid();
    blocked.owner_epoch = blocked.generation.clone();
    blocked.state = "running".into();
    store.save(&blocked).unwrap();

    let mut outside = super::super::tests::request();
    outside.image = "fixture:latest".into();
    outside.name = Some("unrelated".into());
    let outside = store.create(outside).unwrap();
    let volume = store.volume(&format!("{}_data", project.name)).unwrap();
    fs::write(volume.join("sentinel"), b"retain").unwrap();
    let network = format!("{}_default", project.name);

    let blocked_dir = store.directory(&blocked.id);
    let owner = lease_file(&blocked_dir.join("owner.lock")).unwrap();
    owner.lock_exclusive().unwrap();
    let blocked_request = blocked_dir.join("stop-request.json");
    fs::create_dir(&blocked_request).unwrap();

    let error = Project::down_project_only(&store, &project.name, None).unwrap_err();
    assert!(error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::IsADirectory)
    }));
    assert!(!store.directory(&first.id).exists());
    assert!(blocked_dir.is_dir());
    assert!(store.directory(&outside.id).is_dir());
    assert!(store.network_instances(&network).is_ok());
    assert_eq!(fs::read(volume.join("sentinel")).unwrap(), b"retain");

    fs::remove_dir(blocked_request).unwrap();
    drop(owner);
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let retry = || {
            barrier.wait();
            Project::down_project_only(&store, &project.name, None)
        };
        let first = scope.spawn(retry);
        let second = scope.spawn(retry);
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
    });
    assert!(!blocked_dir.exists());
    assert!(store.directory(&outside.id).is_dir());
    assert!(store.network_instances(&network).is_err());
    assert_eq!(fs::read(volume.join("sentinel")).unwrap(), b"retain");
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[rstest::rstest]
#[case::pre_rename("prepared")]
#[case::plan_already_consumed("empty")]
#[case::directory_deleted_first("deleted")]
fn project_only_down_recovers_retirement_boundaries(#[case] phase: &str) {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let project = load(
        directory.path(),
        "services: {app: {image: 'fixture:latest'}}",
    )
    .unwrap();
    let member = reconcile(&project, &store).remove(0);
    let active = store.directory(&member.id);
    let retired = store.retired_directory(&member.id);
    private_directory(&store.retired_root()).unwrap();
    store.prepare_retired_images(&member, &active).unwrap();
    let receipt = store
        .prepare_compose_retirement(&member, member.compose.as_ref().unwrap(), &active)
        .unwrap();
    if phase != "prepared" {
        fs::rename(&active, &retired).unwrap();
        if phase == "empty" {
            for entry in fs::read_dir(&retired).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    fs::remove_dir_all(entry.path()).unwrap();
                } else {
                    fs::remove_file(entry.path()).unwrap();
                }
            }
        } else {
            fs::remove_dir_all(&retired).unwrap();
        }
    }
    let root = store.root.clone();
    drop(store);
    let store = Store::open(root).unwrap();
    if phase == "prepared" {
        // A pre-rename receipt does not grant removal authority to a retry.
        store.finish_project_retirements(&project.name).unwrap();
        assert_eq!(reconcile(&project, &store).remove(0).id, member.id);
        assert!(active.is_dir() && receipt.is_file());
    }
    Project::down_project_only(&store, &project.name, None).unwrap();
    assert!(!active.exists() && !retired.exists() && !receipt.exists());
    Project::down_project_only(&store, &project.name, None).unwrap();
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn compose_down_reaps_only_exact_project_retirements() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let project = load(
        directory.path(),
        "services: {app: {image: 'fixture:latest'}, orphan: {image: 'fixture:latest'}}",
    )
    .unwrap();
    let members = reconcile(&project, &store);
    let member = &members[0];
    let orphan = &members[1];
    let file = directory.path().join("compose.yaml");
    let foreign = Project::load(Some(&file), Some("test-foreign")).unwrap();
    let foreign_members = reconcile(&foreign, &store);
    let foreign_member = &foreign_members[0];
    let mut standalone = super::super::tests::request();
    standalone.image = "fixture:latest".into();
    standalone.name = Some("test-standalone-1".into());
    standalone.labels = BTreeMap::from([
        ("com.docker.compose.project".into(), project.name.clone()),
        ("com.docker.compose.service".into(), "app".into()),
    ]);
    let standalone = store.create(standalone).unwrap();
    private_directory(&store.retired_root()).unwrap();
    let retire = |instance: &Instance, record_receipt: bool| {
        let active = store.directory(&instance.id);
        store.prepare_retired_images(instance, &active).unwrap();
        let receipt = record_receipt.then(|| {
            store
                .prepare_compose_retirement(instance, instance.compose.as_ref().unwrap(), &active)
                .unwrap()
        });
        let retired = store.retired_directory(&instance.id);
        fs::rename(active, &retired).unwrap();
        let record = retired.join(crate::runtime::session_execution::STATE);
        fs::remove_file(record).unwrap();
        (retired, receipt)
    };
    let (selected_dir, receipt) = retire(member, true);
    let receipt = receipt.unwrap();
    let (foreign_dir, foreign_receipt) = retire(foreign_member, true);
    let (standalone_dir, _) = retire(&standalone, false);
    // A legacy plan has no project-scoped authority, even if an old record survived.
    let legacy = &foreign_members[1];
    let legacy_active = store.directory(&legacy.id);
    store
        .prepare_retired_images(legacy, &legacy_active)
        .unwrap();
    let legacy_dir = store.retired_directory(&legacy.id);
    fs::rename(legacy_active, &legacy_dir).unwrap();

    let changed = load(
        directory.path(),
        "services: {new: {image: 'fixture:latest'}}",
    )
    .unwrap();
    let original_bytes = fs::read(&receipt).unwrap();
    fs::write(&receipt, b"{").unwrap();
    let error = changed.down(&store, None, false, false).unwrap_err();
    assert!(format!("{error:#}").contains("Compose retirement receipt"));
    assert!(selected_dir.join(RETIRED_IMAGES).is_file());
    fs::write(&receipt, original_bytes).unwrap();

    let relocated = directory.path().join("relocated");
    fs::rename(&selected_dir, &relocated).unwrap();
    private_directory(&selected_dir).unwrap();
    let error = changed.down(&store, None, false, false).unwrap_err();
    assert!(format!("{error:#}").contains("retired directory identity changed"));
    assert!(selected_dir.is_dir());
    fs::remove_dir(&selected_dir).unwrap();
    fs::rename(relocated, &selected_dir).unwrap();

    let error = changed.down(&store, None, false, false).unwrap_err();
    assert!(format!("{error:#}").contains("network has attached containers"));
    assert!(!selected_dir.exists() && !receipt.exists());
    assert!(store.directory(&orphan.id).is_dir());
    assert!(store.network_instances("test-foreign_default").is_ok());
    assert!(foreign_dir.join(RETIRED_IMAGES).is_file());
    assert!(foreign_receipt.unwrap().is_file());
    assert!(standalone_dir.join(RETIRED_IMAGES).is_file());
    assert!(legacy_dir.join(RETIRED_IMAGES).is_file());
    changed.down(&store, None, true, false).unwrap();
    Project::down_project_only(&store, &foreign.name, None).unwrap();
    assert!(!foreign_dir.exists());
    assert!(legacy_dir.is_dir());
    store.remove_retired(&legacy.id).unwrap();
    store.remove_retired(&standalone.id).unwrap();
}

#[test]
fn compose_empty_stop_signal_inherits_image_and_has_the_unset_config_identity() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let mut catalog = store.catalog().unwrap();
    catalog.values_mut().next().unwrap().stop_signal = 3;
    store.save_catalog(&catalog).unwrap();
    let unset = load(
        directory.path(),
        "services: {app: {image: 'fixture:latest'}}",
    )
    .unwrap();
    for value in ["''", "null"] {
        let project = load(
            directory.path(),
            &format!("services: {{app: {{image: 'fixture:latest', stop_signal: {value}}}}}"),
        )
        .unwrap();
        assert_eq!(
            project.config_hash("app").unwrap(),
            unset.config_hash("app").unwrap()
        );
        let instance = reconcile(&project, &store).remove(0);
        assert!(instance.config.stop_signal.is_none());
        assert_eq!(instance.image.stop_signal, 3);
    }
}

#[test]
fn compose_stop_signal_validates_and_keeps_the_admitted_override() {
    let directory = tempfile::tempdir().unwrap();
    for spelling in ["SIGINT", "INT", "2", "SIGTERM"] {
        let project = load(
            directory.path(),
            &format!("services: {{app: {{image: 'fixture:latest', stop_signal: '{spelling}'}}}}"),
        )
        .unwrap();
        assert!(project.config().unwrap().contains(spelling));
    }
    for invalid in ["0", "-1", "SIGZERO", "1000"] {
        assert!(
            load(
                directory.path(),
                &format!(
                    "services: {{app: {{image: 'fixture:latest', stop_signal: '{invalid}'}}}}"
                ),
            )
            .is_err()
        );
    }
    let store = store(directory.path());
    let project = load(
        directory.path(),
        "services: {app: {image: 'fixture:latest', stop_signal: SIGINT}}",
    )
    .unwrap();
    let first = reconcile(&project, &store).remove(0);
    assert_eq!(first.config.stop_signal, Some(2));
    assert_eq!(
        store.resolve(&first.id).unwrap().image.stop_signal,
        store.images().unwrap()[0].stop_signal,
        "service settings must not rewrite image metadata"
    );
    let changed = load(
        directory.path(),
        "services: {app: {image: 'fixture:latest', stop_signal: SIGTERM}}",
    )
    .unwrap();
    let retained = changed
        .reconcile(
            &store,
            &[],
            &UpOptions {
                recreate: RecreatePolicy::Never,
                ..UpOptions::default()
            },
            &mut |_| {},
        )
        .unwrap()
        .remove(0);
    assert_eq!(retained.id, first.id);
    assert_eq!(retained.config.stop_signal, Some(2));
    let replaced = reconcile(&changed, &store).remove(0);
    assert_ne!(replaced.id, first.id);
    assert_eq!(replaced.config.stop_signal, Some(15));
}

#[test]
fn compose_command_and_entrypoint_preserve_inherit_clear_and_replace() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let mut image = store.images().unwrap().remove(0);
    image.entrypoint = vec!["image-entry".into(), "image-flag".into()];
    image.cmd = vec!["image-cmd".into(), "image-arg".into()];
    for (index, (fields, expected)) in [
        (
            "",
            vec!["image-entry", "image-flag", "image-cmd", "image-arg"],
        ),
        (
            "command: null",
            vec!["image-entry", "image-flag", "image-cmd", "image-arg"],
        ),
        ("command: []", vec!["image-entry", "image-flag"]),
        ("command: ''", vec!["image-entry", "image-flag"]),
        ("command: ['']", vec!["image-entry", "image-flag", ""]),
        (
            "command: [arg, 'two words']",
            vec!["image-entry", "image-flag", "arg", "two words"],
        ),
        (
            "entrypoint: null",
            vec!["image-entry", "image-flag", "image-cmd", "image-arg"],
        ),
        ("entrypoint: [new, flag]", vec!["new", "flag"]),
        ("entrypoint: 'new \"two words\"'", vec!["new", "two words"]),
        (
            "entrypoint: [new, flag]\ncommand: null",
            vec!["new", "flag"],
        ),
        ("entrypoint: [new, flag]\ncommand: []", vec!["new", "flag"]),
        (
            "entrypoint: [new, flag]\ncommand: [arg, '']",
            vec!["new", "flag", "arg", ""],
        ),
        (
            "entrypoint: []\ncommand: [direct, arg]",
            vec!["direct", "arg"],
        ),
        (
            "entrypoint: ''\ncommand: 'direct \"two words\"'",
            vec!["direct", "two words"],
        ),
        (
            "entrypoint: null\ncommand: [arg]",
            vec!["image-entry", "image-flag", "arg"],
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let fields = fields.replace('\n', "\n    ");
        let project = load(
            directory.path(),
            &format!("services:\n  app:\n    image: fixture:latest\n    {fields}\n"),
        )
        .unwrap();
        let mut config = project.create_request("app", &store, &image).unwrap();
        config.name = Some(format!("argv-{index}"));
        config.network = false;
        config.network_name = None;
        let instance = store
            .create_pinned_member(config, &mut |_| {}, None, Some(image.clone()))
            .unwrap();
        assert_eq!(instance.argv, expected, "{fields}");
        assert_eq!(
            store.resolve(&instance.id).unwrap().argv,
            expected,
            "{fields}"
        );
    }
    for fields in [
        "entrypoint: []",
        "entrypoint: ''",
        "entrypoint: []\ncommand: []",
    ] {
        let fields = fields.replace('\n', "\n    ");
        let project = load(
            directory.path(),
            &format!("services:\n  app:\n    image: fixture:latest\n    {fields}\n"),
        )
        .unwrap();
        assert!(
            project
                .create_request("app", &store, &image)
                .unwrap_err()
                .to_string()
                .contains("No command specified")
        );
    }
}

#[test]
fn compose_empty_effective_command_fails_before_creating_project_resources() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let project = load(directory.path(),
        "services: {app: {image: 'fixture:latest', command: [], volumes: ['data:/data']}}\nvolumes: {data: {}}").unwrap();
    let error = project
        .reconcile(&store, &[], &UpOptions::default(), &mut |_| {})
        .unwrap_err();
    assert!(
        error.to_string().contains("No command specified"),
        "{error:#}"
    );
    assert!(store.instances(true).unwrap().is_empty());
    assert!(
        !store
            .networks()
            .unwrap()
            .iter()
            .any(|network| network.name == "test_default")
    );
    assert!(!store.root.join("volumes/test_data").exists());
}

#[test]
fn compose_named_volume_data_survives_selective_recreation_and_default_down() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let project = load(
        directory.path(),
        r#"
services:
  app:
    image: fixture:latest
    environment: {VERSION: old}
    volumes: [data:/data]
  db:
    image: fixture:latest
volumes:
  data:
"#,
    )
    .unwrap();
    let first = reconcile(&project, &store);
    let app = first
        .iter()
        .find(|instance| instance.compose.as_ref().unwrap().service == "app")
        .unwrap();
    let db = first
        .iter()
        .find(|instance| instance.compose.as_ref().unwrap().service == "db")
        .unwrap();
    let data = store.root.join("volumes/test_data");
    fs::write(data.join("keep"), b"persistent bytes").unwrap();
    assert_eq!(
        reconcile(&project, &store)
            .iter()
            .map(|instance| &instance.id)
            .collect::<Vec<_>>(),
        first
            .iter()
            .map(|instance| &instance.id)
            .collect::<Vec<_>>()
    );
    let mut changed = project;
    changed.document.services.get_mut("app").unwrap().env_file = None;
    changed
        .document
        .services
        .get_mut("app")
        .unwrap()
        .environment = Environment::Map(BTreeMap::from([("VERSION".into(), Some("new".into()))]));
    let updated = reconcile(&changed, &store);
    let new_app = updated
        .iter()
        .find(|instance| instance.compose.as_ref().unwrap().service == "app")
        .unwrap();
    assert_ne!(new_app.id, app.id);
    assert_eq!(new_app.name, app.name);
    assert_eq!(new_app.env["VERSION"], "new");
    assert_eq!(new_app.config.mounts[0].source, data);
    assert!(updated.iter().any(|instance| instance.id == db.id));
    assert_eq!(fs::read(data.join("keep")).unwrap(), b"persistent bytes");
    changed.down(&store, Duration::ZERO, false, false).unwrap();
    assert_eq!(fs::read(data.join("keep")).unwrap(), b"persistent bytes");
    reconcile(&changed, &store);
    changed.down(&store, Duration::ZERO, false, true).unwrap();
    assert!(!data.exists());
    changed.down(&store, Duration::ZERO, false, true).unwrap();
}

#[test]
fn compose_down_volumes_preserves_external_and_refuses_foreign_references() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let external = store.volume("shared").unwrap();
    fs::write(external.join("keep"), b"external").unwrap();
    let project = load(
        directory.path(),
        r#"
services:
  app:
    image: fixture:latest
    volumes: [data:/data, shared:/shared]
volumes:
  data: {}
  shared: {external: true}
"#,
    )
    .unwrap();
    reconcile(&project, &store);
    let mut config = project
        .create_request("app", &store, &store.images().unwrap()[0])
        .unwrap();
    config.name = Some("foreign".into());
    let foreign = store.create(config).unwrap();
    assert!(
        project
            .down(&store, Duration::ZERO, false, true)
            .unwrap_err()
            .to_string()
            .contains("still referenced")
    );
    assert!(store.root.join("volumes/test_data").is_dir());
    assert_eq!(fs::read(external.join("keep")).unwrap(), b"external");
    store.remove(&foreign.id, false).unwrap();
    project.down(&store, Duration::ZERO, false, true).unwrap();
    assert!(!store.root.join("volumes/test_data").exists());
    assert_eq!(fs::read(external.join("keep")).unwrap(), b"external");
}

#[test]
fn compose_down_volumes_only_removes_current_declarations_not_historical_names() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let previous_text = "services: {app: {image: 'fixture:latest', volumes: ['data:/data']}}\nvolumes: {data: {name: archive-data}}";
    let previous = load(directory.path(), previous_text).unwrap();
    let archive = reconcile(&previous, &store)[0].config.mounts[0]
        .source
        .clone();
    fs::write(archive.join("keep"), b"historical data").unwrap();
    let current = load(directory.path(),
        "services: {app: {image: 'fixture:latest', volumes: ['data:/data']}}\nvolumes: {data: {name: current-data}}").unwrap();
    let current_data = reconcile(&current, &store)[0].config.mounts[0]
        .source
        .clone();
    fs::write(current_data.join("keep"), b"current data").unwrap();
    current.down(&store, Duration::ZERO, true, true).unwrap();
    assert!(!current_data.exists());
    assert_eq!(fs::read(archive.join("keep")).unwrap(), b"historical data");
    // Historical ownership must survive too: explicitly declaring the old
    // name again can reuse it, and only then authorize its removal.
    let declared_again = load(directory.path(), previous_text).unwrap();
    assert_eq!(
        reconcile(&declared_again, &store)[0].config.mounts[0].source,
        archive
    );
    declared_again
        .down(&store, Duration::ZERO, false, true)
        .unwrap();
    assert!(!archive.exists());
}

#[test]
fn compose_down_volumes_without_declarations_retains_previous_data() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let previous = load(
        directory.path(),
        "services: {app: {image: 'fixture:latest', volumes: ['data:/data']}}\nvolumes: {data: {}}",
    )
    .unwrap();
    let data = reconcile(&previous, &store)[0].config.mounts[0]
        .source
        .clone();
    fs::write(data.join("keep"), b"not selected for deletion").unwrap();
    let current = load(
        directory.path(),
        "services: {app: {image: 'fixture:latest'}}",
    )
    .unwrap();
    current.down(&store, Duration::ZERO, true, true).unwrap();
    assert!(store.instances(true).unwrap().is_empty());
    assert_eq!(
        fs::read(data.join("keep")).unwrap(),
        b"not selected for deletion"
    );
}

#[test]
fn compose_down_volume_declarations_do_not_grant_ownership_or_override_external() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let previous = load(directory.path(),
        "services: {app: {image: 'fixture:latest', volumes: ['data:/data']}}\nvolumes: {data: {name: owned-data}}").unwrap();
    let owned = reconcile(&previous, &store)[0].config.mounts[0]
        .source
        .clone();
    fs::write(owned.join("keep"), b"owned bytes").unwrap();
    previous.down(&store, Duration::ZERO, false, false).unwrap();
    let ordinary = store.volume("ordinary-data").unwrap();
    fs::write(ordinary.join("keep"), b"ordinary bytes").unwrap();
    let current = load(
        directory.path(),
        r#"
services: {app: {image: 'fixture:latest'}}
volumes:
  data: {name: owned-data}
  ordinary: {name: ordinary-data}
"#,
    )
    .unwrap();
    assert!(
        current
            .down(&store, Duration::ZERO, false, true)
            .unwrap_err()
            .to_string()
            .contains("unowned volume")
    );
    // Scope and ownership checks finish before deleting any selected named data.
    assert_eq!(fs::read(owned.join("keep")).unwrap(), b"owned bytes");
    assert_eq!(fs::read(ordinary.join("keep")).unwrap(), b"ordinary bytes");
    let external = load(
        directory.path(),
        r#"
services: {app: {image: 'fixture:latest'}}
volumes:
  data: {name: owned-data, external: true}
  ordinary: {name: ordinary-data, external: true}
"#,
    )
    .unwrap();
    external.down(&store, Duration::ZERO, false, true).unwrap();
    assert_eq!(fs::read(owned.join("keep")).unwrap(), b"owned bytes");
    assert_eq!(fs::read(ordinary.join("keep")).unwrap(), b"ordinary bytes");
    let declared_again = load(
        directory.path(),
        "services: {app: {image: 'fixture:latest'}}\nvolumes: {data: {name: owned-data}}",
    )
    .unwrap();
    let other = Project::load(Some(&directory.path().join("compose.yaml")), Some("other")).unwrap();
    assert!(
        other
            .down(&store, Duration::ZERO, false, true)
            .unwrap_err()
            .to_string()
            .contains("another owner")
    );
    assert_eq!(fs::read(owned.join("keep")).unwrap(), b"owned bytes");
    declared_again
        .down(&store, Duration::ZERO, false, true)
        .unwrap();
    assert!(!owned.exists());
    assert_eq!(fs::read(ordinary.join("keep")).unwrap(), b"ordinary bytes");
}

#[test]
fn compose_volume_ownership_rejects_adoption_other_projects_and_missing_external_data() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let unowned = store.volume("existing").unwrap();
    fs::write(unowned.join("keep"), b"unowned").unwrap();
    for definition in ["{name: existing}", "{external: true, name: missing}"] {
        let project = load(directory.path(), &format!("services: {{app: {{image: 'fixture:latest', volumes: ['data:/data']}}}}\nvolumes: {{data: {definition}}}")).unwrap();
        assert!(
            project
                .reconcile(&store, &[], &UpOptions::default(), &mut |_| {})
                .is_err()
        );
        assert!(store.instances(true).unwrap().is_empty());
    }
    let project = load(directory.path(), "services: {app: {image: 'fixture:latest', volumes: ['data:/data']}}\nvolumes: {data: {name: owned}}").unwrap();
    reconcile(&project, &store);
    let other = Project::load(Some(&directory.path().join("compose.yaml")), Some("other")).unwrap();
    assert!(
        other
            .reconcile(&store, &[], &UpOptions::default(), &mut |_| {})
            .unwrap_err()
            .to_string()
            .contains("another owner")
    );
    assert_eq!(fs::read(unowned.join("keep")).unwrap(), b"unowned");
    assert!(store.root.join("volumes/owned").exists());
}

#[test]
fn compose_build_generates_local_tags_and_image_changes_recreate_with_anonymous_data() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let context = directory.path().join("context");
    fs::write(context.join("Dockerfile"), "FROM scratch\nARG MESSAGE=default\nENV MESSAGE=$MESSAGE\nVOLUME /anon\nCMD [\"/bin/true\"]\n").unwrap();
    let project = load(
        directory.path(),
        "services: {app: {build: {context: ./context, args: {MESSAGE: first}}}}",
    )
    .unwrap();
    // Missing images build automatically. No RUN means no virtualization needed.
    let initial = reconcile(&project, &store);
    let first = &initial[0];
    assert_eq!(first.image.reference, "test-app:latest");
    assert_eq!(first.env["MESSAGE"], "first");
    let anon = first
        .config
        .mounts
        .iter()
        .find(|mount| mount.target == Path::new("/anon"))
        .unwrap();
    fs::write(anon.source.join("keep"), b"anonymous bytes").unwrap();
    assert_eq!(reconcile(&project, &store)[0].id, first.id);
    let changed = load(
        directory.path(),
        "services: {app: {build: {context: ./context, args: {MESSAGE: second}}}}",
    )
    .unwrap();
    // Changing only a producer definition does not replace a still-pinned image.
    assert_eq!(reconcile(&changed, &store)[0].id, first.id);
    changed
        .build(&store, &[], &BuildSettings::default())
        .unwrap();
    let replacement = reconcile(&changed, &store).remove(0);
    assert_ne!(replacement.id, first.id);
    assert_eq!(replacement.env["MESSAGE"], "second");
    assert_eq!(replacement.config.mounts[0].source, anon.source);
    assert_eq!(
        fs::read(anon.source.join("keep")).unwrap(),
        b"anonymous bytes"
    );
    fs::write(
        context.join("Dockerfile"),
        "FROM scratch\nNOT_SUPPORTED nope\n",
    )
    .unwrap();
    assert!(
        changed
            .build(&store, &[], &BuildSettings::default())
            .is_err()
    );
    assert_eq!(
        store.resolve(&replacement.id).unwrap().image.id,
        replacement.image.id
    );
    assert_eq!(
        store.local_image("test-app:latest").unwrap().unwrap().id,
        replacement.image.id
    );
    changed.down(&store, Duration::ZERO, false, true).unwrap();
    assert!(!anon.source.exists());
}

#[test]
fn compose_replacement_admission_failure_keeps_all_old_members_and_retry_cleans_pending() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let mut project = load(
        directory.path(),
        "services: {app: {image: 'fixture:latest'}, other: {image: 'fixture:latest'}}",
    )
    .unwrap();
    let initial = reconcile(&project, &store);
    project.document.services.get_mut("app").unwrap().command =
        Some(Words::List(vec!["changed".into()]));
    // Inject an invalid request below the YAML loader to check runtime admission.
    project
        .document
        .services
        .get_mut("other")
        .unwrap()
        .working_dir = Some("relative".into());
    assert!(
        project
            .reconcile(&store, &[], &UpOptions::default(), &mut |_| {})
            .is_err()
    );
    for instance in &initial {
        assert!(store.resolve(&instance.id).is_ok());
    }
    assert!(store.instances(true).unwrap().iter().any(|instance| {
        instance
            .compose
            .as_ref()
            .is_some_and(|member| member.replaces.is_some())
    }));
    let pending = store
        .instances(true)
        .unwrap()
        .into_iter()
        .find(|instance| {
            instance
                .compose
                .as_ref()
                .is_some_and(|member| member.replaces.is_some())
        })
        .unwrap();
    assert!(
        store
            .start(&pending.id)
            .unwrap_err()
            .to_string()
            .contains("cannot be started directly")
    );
    let mut invalid = pending.clone();
    invalid.state = "exited".into();
    invalid.generation = uuid();
    invalid.owner_epoch = uuid();
    invalid.exit_code = Some(1);
    store.save(&invalid).unwrap();
    let error = Project::down_project_only(&store, &project.name, None).unwrap_err();
    assert!(error.to_string().contains("unexpected lifecycle"));
    assert_eq!(store.instances(true).unwrap().len(), 3);
    store.save(&pending).unwrap();

    let mut unrelated = super::super::tests::request();
    unrelated.image = "fixture:latest".into();
    unrelated.name = Some(pending.id.clone());
    let mut unrelated = store.create(unrelated).unwrap();
    unrelated.created_ms = pending.created_ms.saturating_add(1);
    store.save(&unrelated).unwrap();

    project
        .document
        .services
        .get_mut("other")
        .unwrap()
        .working_dir = None;
    let updated = reconcile(&project, &store);
    assert_eq!(updated.len(), 2);
    assert_eq!(store.instances(true).unwrap().len(), 3);
    assert!(store.find_id(&pending.id).unwrap().is_none());
    assert!(store.find_id(&unrelated.id).unwrap().is_some());
    assert!(
        updated
            .iter()
            .all(|instance| instance.compose.as_ref().unwrap().replaces.is_none())
    );
}

#[test]
fn compose_no_deps_force_recreate_and_no_build_are_explicit() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let project = load(directory.path(), "services: {app: {image: 'fixture:latest', depends_on: [db]}, db: {image: 'fixture:latest'}}").unwrap();
    let options = UpOptions {
        dependencies: false,
        ..Default::default()
    };
    let first = project
        .reconcile(&store, &["app".into()], &options, &mut |_| {})
        .unwrap();
    assert_eq!(store.instances(true).unwrap().len(), 1);
    let forced = project
        .reconcile(
            &store,
            &["app".into()],
            &UpOptions {
                recreate: RecreatePolicy::Always,
                ..options
            },
            &mut |_| {},
        )
        .unwrap();
    assert_ne!(forced[0].id, first[0].id);
    assert_eq!(store.instances(true).unwrap().len(), 1);
    let project = load(
        directory.path(),
        "services: {buildonly: {build: ./context}}",
    )
    .unwrap();
    assert!(
        project
            .reconcile(
                &store,
                &[],
                &UpOptions {
                    build: BuildPolicy::Never,
                    ..Default::default()
                },
                &mut |_| {}
            )
            .unwrap_err()
            .to_string()
            .contains("no local image")
    );
    assert!(
        store
            .local_image("test-buildonly:latest")
            .unwrap()
            .is_none()
    );
}

#[test]
fn compose_interrupted_promotion_recovers_admitted_candidate_without_losing_anonymous_data() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    fs::write(
        directory.path().join("context/Dockerfile"),
        "FROM scratch\nVOLUME /anon\nCMD [\"/bin/true\"]\n",
    )
    .unwrap();
    let mut project = load(
        directory.path(),
        "services: {app: {build: ./context}, other: {image: 'fixture:latest'}}",
    )
    .unwrap();
    let initial = reconcile(&project, &store);
    let app = initial
        .iter()
        .find(|instance| instance.compose.as_ref().unwrap().service == "app")
        .unwrap();
    let anon = app.config.mounts[0].source.clone();
    fs::write(anon.join("keep"), b"retained across interrupted promotion").unwrap();
    project.document.services.get_mut("app").unwrap().command =
        Some(Words::List(vec!["changed".into()]));
    project
        .document
        .services
        .get_mut("other")
        .unwrap()
        .working_dir = Some("relative".into());
    assert!(
        project
            .reconcile(&store, &[], &UpOptions::default(), &mut |_| {})
            .is_err()
    );
    let pending = store
        .instances(true)
        .unwrap()
        .into_iter()
        .find(|instance| {
            instance
                .compose
                .as_ref()
                .is_some_and(|member| member.replaces.as_deref() == Some(&app.id))
        })
        .unwrap();
    // Exercise the persisted state between old-record removal and final
    // promotion publication. This is a metadata fault fixture, not a VM crash.
    store.remove(&app.id, false).unwrap();
    project
        .document
        .services
        .get_mut("other")
        .unwrap()
        .working_dir = None;
    let recovered = reconcile(&project, &store);
    let app = recovered
        .iter()
        .find(|instance| instance.compose.as_ref().unwrap().service == "app")
        .unwrap();
    assert_eq!(app.id, pending.id);
    assert_eq!(app.name, "test-app-1");
    assert!(app.compose.as_ref().unwrap().replaces.is_none());
    assert_eq!(app.config.mounts[0].source, anon);
    assert_eq!(
        fs::read(anon.join("keep")).unwrap(),
        b"retained across interrupted promotion"
    );
}

#[test]
fn compose_volume_removal_rejects_changed_symlink_authority() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let project = load(
        directory.path(),
        "services: {app: {image: 'fixture:latest', volumes: ['data:/data']}}\nvolumes: {data: {}}",
    )
    .unwrap();
    reconcile(&project, &store);
    project.down(&store, Duration::ZERO, false, false).unwrap();
    let data = store.root.join("volumes/test_data");
    let saved = store.root.join("volumes/saved-data");
    fs::rename(&data, &saved).unwrap();
    std::os::unix::fs::symlink(directory.path().join("does-not-exist"), &data).unwrap();
    assert!(project.down(&store, Duration::ZERO, false, true).is_err());
    assert!(
        fs::symlink_metadata(&data)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(saved.is_dir());
    fs::remove_file(&data).unwrap();
    fs::create_dir(&data).unwrap();
    fs::write(data.join("foreign"), b"replacement directory bytes").unwrap();
    assert!(
        project
            .down(&store, Duration::ZERO, false, true)
            .unwrap_err()
            .to_string()
            .contains("identity changed")
    );
    assert_eq!(
        fs::read(data.join("foreign")).unwrap(),
        b"replacement directory bytes"
    );
}

#[test]
fn compose_health_dependencies_validate_before_container_creation() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let project = load(
        directory.path(),
        r#"
services:
  db: {image: "fixture:latest"}
  app:
    image: fixture:latest
    depends_on:
      db: {condition: service_healthy}
"#,
    )
    .unwrap();
    assert!(
        project
            .reconcile(&store, &[], &UpOptions::default(), &mut |_| {})
            .unwrap_err()
            .to_string()
            .contains("has no healthcheck")
    );
    assert!(store.instances(true).unwrap().is_empty());
    let project = load(
        directory.path(),
        r#"
services:
  db:
    image: fixture:latest
    healthcheck:
      test: [CMD, "true"]
      interval: 100ms
  migration:
    image: fixture:latest
    depends_on: {db: {condition: service_healthy}}
  app:
    image: fixture:latest
    depends_on: {migration: {condition: service_completed_successfully}}
"#,
    )
    .unwrap();
    assert_eq!(
        project.order(&[], true).unwrap(),
        ["db", "migration", "app"]
    );
    let instances = reconcile(&project, &store);
    let db = instances
        .iter()
        .find(|instance| instance.name == "test-db-1")
        .unwrap();
    assert!(db.config.healthcheck.is_some());
    assert!(
        store.health(&db.id).unwrap().is_none(),
        "stopped is not healthy"
    );
    assert!(project.is_completion_job("migration"));
}

#[test]
fn compose_file_bind_admission_exposes_no_host_parent() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let file = directory.path().join("config.json");
    fs::write(&file, b"{\"value\":1}").unwrap();
    let project = load(
        directory.path(),
        r#"
services:
  app:
    image: fixture:latest
    volumes: ["./config.json:/app/config.json:ro"]
"#,
    )
    .unwrap();
    let instances = reconcile(&project, &store);
    let mount = &instances[0].config.mounts[0];
    assert_eq!(mount.source, file);
    assert!(mount.read_only);
    assert_eq!(
        fs::metadata(&file).unwrap().nlink(),
        1,
        "admission probe must release its temporary pin"
    );
}

#[test]
fn compose_started_dependencies_do_not_require_liveness_but_wait_conditions_do() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let project = load(
        directory.path(),
        r#"
services:
  job:
    image: fixture:latest
    healthcheck: {test: [CMD, "true"]}
  short:
    image: fixture:latest
    depends_on: [job]
  started:
    image: fixture:latest
    depends_on: {job: {condition: service_started}}
  healthy:
    image: fixture:latest
    depends_on: {job: {condition: service_healthy}}
  completed:
    image: fixture:latest
    depends_on: {job: {condition: service_completed_successfully}}
"#,
    )
    .unwrap();
    let instances = reconcile(&project, &store);
    let mut job = instances
        .iter()
        .find(|instance| instance.name == "test-job-1")
        .unwrap()
        .clone();
    // Persisted exit fixtures, not simulated VM startup. Both forms of Started
    // only order startup; Healthy/Completed still consume current observations.
    job.generation = uuid();
    job.owner_epoch = job.generation.clone();
    for code in [0, 17] {
        job.state = "exited".into();
        job.exit_code = Some(code);
        store.save(&job).unwrap();
        for service in ["short", "started"] {
            project
                .wait_dependencies(&store, service, Instant::now())
                .unwrap();
        }
        assert!(
            project
                .wait_dependencies(&store, "healthy", Instant::now())
                .is_err()
        );
        assert_eq!(
            project
                .wait_dependencies(&store, "completed", Instant::now())
                .is_ok(),
            code == 0
        );
    }
    let mut app = instances
        .into_iter()
        .find(|instance| instance.name == "test-short-1")
        .unwrap();
    app.generation = uuid();
    app.owner_epoch = app.generation.clone();
    app.state = "exited".into();
    app.exit_code = Some(0);
    store.save(&app).unwrap();
    assert!(
        project.wait_ready(&store, &app, Instant::now()).is_err(),
        "--wait still requires a non-job service without health checks to be running"
    );
}

#[test]
fn compose_health_observations_never_reuse_an_old_launch_generation() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let project = load(
        directory.path(),
        r#"
services:
  app:
    image: fixture:latest
    healthcheck: {test: [CMD, "true"]}
"#,
    )
    .unwrap();
    let mut instance = reconcile(&project, &store).remove(0);
    // Reader contract fixture: emulate an owned generation without booting a VM.
    let lease = lease_file(&store.directory(&instance.id).join("owner.lock")).unwrap();
    lease.lock_exclusive().unwrap();
    instance.state = "running".into();
    instance.generation = "current-generation".into();
    store.save(&instance).unwrap();
    let health_file = store.directory(&instance.id).join("health.json");
    let mut status = health::Status {
        generation: "previous-generation".into(),
        state: health::State::Healthy,
        failing_streak: 0,
        exit_code: Some(0),
        output: String::new(),
    };
    crate::util::write_private_json(&health_file, &status).unwrap();
    let observed = store.health(&instance.id).unwrap().unwrap();
    assert_eq!(observed.generation, instance.generation);
    assert_eq!(observed.state, health::State::Starting);
    status.generation = instance.generation.clone();
    crate::util::write_private_json(&health_file, &status).unwrap();
    assert_eq!(
        store.health(&instance.id).unwrap().unwrap().state,
        health::State::Healthy
    );
    fs::write(&health_file, b"corrupt").unwrap();
    assert!(store.health(&instance.id).is_err());
    instance.state = "exited".into();
    store.save(&instance).unwrap();
    assert!(store.health(&instance.id).unwrap().is_none());
}

#[test]
fn readiness_follows_retry_executions_but_never_a_replaced_session_owner_or_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let project = load(
        directory.path(),
        "services:\n  app:\n    image: fixture:latest\n    healthcheck: {test: [CMD, 'true']}\n",
    )
    .unwrap();
    let mut instance = reconcile(&project, &store).remove(0);
    let lease = lease_file(&store.directory(&instance.id).join("owner.lock")).unwrap();
    lease.lock_exclusive().unwrap();
    instance.state = "running".into();
    instance.generation = uuid();
    instance.owner_epoch = instance.generation.clone();
    store.save(&instance).unwrap();
    let selected = instance.clone();
    let path = store.directory(&instance.id).join("health.json");
    let mut health = health::Status {
        generation: instance.generation.clone(),
        state: health::State::Healthy,
        failing_streak: 0,
        exit_code: Some(0),
        output: String::new(),
    };
    crate::util::write_private_json(&path, &health).unwrap();
    project
        .wait_ready(&store, &selected, Instant::now())
        .unwrap();
    instance.owner_epoch = uuid();
    store.save(&instance).unwrap();
    assert!(
        project
            .wait_ready(&store, &selected, Instant::now())
            .unwrap_err()
            .to_string()
            .contains("changed Session owner/configuration")
    );
    instance.owner_epoch = selected.owner_epoch.clone();
    instance
        .compose
        .as_mut()
        .unwrap()
        .config_hash
        .push_str("-changed");
    store.save(&instance).unwrap();
    assert!(
        project
            .wait_ready(&store, &selected, Instant::now())
            .unwrap_err()
            .to_string()
            .contains("changed Session owner/configuration")
    );
    instance.compose = selected.compose.clone();
    instance.generation = uuid();
    instance.restart_count = 1;
    store.save(&instance).unwrap();
    assert!(
        project
            .wait_ready(&store, &selected, Instant::now())
            .unwrap_err()
            .to_string()
            .contains("readiness timeout"),
        "old health must not satisfy a retry"
    );
    health.generation = instance.generation.clone();
    crate::util::write_private_json(&path, &health).unwrap();
    project
        .wait_ready(&store, &selected, Instant::now())
        .unwrap();
}
