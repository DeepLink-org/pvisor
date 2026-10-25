//! First-party discovery is inert; dispatch preserves argv and exit.
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn container_run_and_job_spawn_have_distinct_help_and_argument_contracts() {
    let temporary = tempfile::tempdir().unwrap();
    for (name, expected, absent) in [
        ("run", "--platform", "--safe"),
        ("spawn", "--safe", "--platform"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .args([name, "--help"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let help = String::from_utf8_lossy(&output.stdout);
        assert!(help.contains(expected), "{help}");
        assert!(!help.contains(absent), "{help}");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["run", "--safe", "--", "/bin/true"])
        .env("PVISOR_RUN_HOME", temporary.path().join("jobs"))
        .env("PVISOR_DOCKER_HOME", temporary.path().join("containers"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));
    assert!(!temporary.path().join("jobs").exists());
    assert!(!temporary.path().join("containers").exists());
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["images", "-q"])
        .env("PVISOR_DOCKER_HOME", temporary.path().join("containers"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn discovery_is_inert_and_dispatch_preserves_arguments_and_exit() {
    let temporary = tempfile::tempdir().unwrap();
    let kernel = temporary.path().join("pvisor");
    fs::copy(env!("CARGO_BIN_EXE_pvisor"), &kernel).unwrap();
    let marker = temporary.path().join("executed");
    let plugin = temporary.path().join("pvisor-tui");
    fs::write(
        &plugin,
        format!(
            "#!/bin/sh\n: > '{}'\nprintf '%s\\n' \"$@\"\nexit 42\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&plugin, fs::Permissions::from_mode(0o755)).unwrap();
    let list = Command::new(&kernel)
        .arg("--help")
        .env("PATH", temporary.path())
        .output()
        .unwrap();
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let help = String::from_utf8_lossy(&list.stdout);
    let extensions = help
        .split("Tools:\n")
        .nth(1)
        .unwrap()
        .split("Options:\n")
        .next()
        .unwrap();
    assert!(
        extensions.lines().any(|line| line.starts_with("  tui ")),
        "{help}"
    );
    assert!(!help.contains("\n  extensions "), "{help}");
    assert!(!marker.exists(), "discovery executed the extension");
    let output = Command::new(&kernel)
        .args(["tui", "space argument", "--literal", ""])
        .env("PATH", format!("{}:/bin", temporary.path().display()))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(42));
    assert_eq!(output.stdout, b"space argument\n--literal\n\n");
    assert!(marker.exists());
}

#[test]
fn daemon_companion_is_discovered_inertly_and_forwards_management_commands() {
    let temporary = tempfile::tempdir().unwrap();
    let kernel = temporary.path().join("pvisor");
    fs::copy(env!("CARGO_BIN_EXE_pvisor"), &kernel).unwrap();
    let marker = temporary.path().join("executed");
    let companion = temporary.path().join("pvisor-daemon");
    fs::write(
        &companion,
        format!(
            "#!/bin/sh\n: > '{}'\nprintf '%s\\n' \"$@\"\nexit 42\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&companion, fs::Permissions::from_mode(0o755)).unwrap();
    let help = Command::new(&kernel).arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("  daemon "));
    assert!(!marker.exists());
    for (args, expected) in [
        (
            vec![
                "daemon",
                "start",
                "--memory-pool",
                "--state",
                "space argument",
            ],
            "start\n--memory-pool\n--state\nspace argument\n",
        ),
        (vec!["help", "daemon"], "--help\n"),
        (vec!["daemon", "status"], "status\n"),
        (vec!["daemon", "stop"], "stop\n"),
    ] {
        let output = Command::new(&kernel).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(42));
        assert_eq!(output.stdout, expected.as_bytes());
    }
    assert!(marker.exists());
}

#[test]
fn automatic_pool_selection_reaches_daemon_for_jobs_and_containers_and_propagates_errors() {
    let temporary = tempfile::tempdir().unwrap();
    let kernel = temporary.path().join("pvisor");
    fs::copy(env!("CARGO_BIN_EXE_pvisor"), &kernel).unwrap();
    let marker = temporary.path().join("executed");
    let parent_marker = temporary.path().join("parent");
    let companion = temporary.path().join("pvisor-daemon");
    fs::write(&companion, format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '%s' \"$PPID\" > '{}'\nprintf 'pool-unavailable-canary\\n' >&2\nexit 7\n", marker.display(), parent_marker.display()
    )).unwrap();
    fs::set_permissions(&companion, fs::Permissions::from_mode(0o755)).unwrap();
    for args in [
        vec!["run", "--memory-pool", "auto", "unused-image"],
        vec!["run", "-M", "unused-image"],
        vec!["spawn", "-M", "--rootfs", "/", "--", "/bin/true"],
        vec![
            "spawn",
            "--vm-memory-pool",
            "auto",
            "--rootfs",
            "/",
            "--",
            "/bin/true",
        ],
    ] {
        let child = Command::new(&kernel)
            .args(args)
            .env("PVISOR_DOCKER_HOME", temporary.path().join("containers"))
            .env("PVISOR_RUN_HOME", temporary.path().join("jobs"))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let frontend_pid = child.id();
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("pool-unavailable-canary"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(&marker).unwrap(), b"resolve-memory-pool\n");
        assert_eq!(
            fs::read_to_string(&parent_marker).unwrap(),
            frontend_pid.to_string(),
            "user-wide services must start outside the Job owner's cleanup tree"
        );
        fs::remove_file(&marker).unwrap();
    }
    // Explicit sockets preserve the old fail-closed path and do not bootstrap.
    let socket = temporary.path().join("missing.sock");
    let output = Command::new(&kernel)
        .args(["run", "--memory-pool"])
        .arg(socket)
        .arg("unused-image")
        .env("PVISOR_DOCKER_HOME", temporary.path().join("containers"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!marker.exists());
    let output = Command::new(&kernel)
        .args(["images", "-q"])
        .env("PVISOR_DOCKER_HOME", temporary.path().join("containers"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!marker.exists());
}

#[test]
fn job_pool_startup_remains_cancellable_while_the_companion_waits() {
    use std::time::{Duration, Instant};
    let temporary = tempfile::tempdir().unwrap();
    let binary = temporary.path().join("pvisor");
    fs::copy(env!("CARGO_BIN_EXE_pvisor"), &binary).unwrap();
    let ready = temporary.path().join("ready");
    let release = temporary.path().join("release");
    let done = temporary.path().join("done");
    struct Release(std::path::PathBuf, std::path::PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = fs::write(&self.0, b"");
            let deadline = Instant::now() + Duration::from_secs(2);
            while !self.1.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    let _release = Release(release.clone(), done.clone());
    let companion = temporary.path().join("pvisor-daemon");
    fs::write(
        &companion,
        format!(
            "#!/bin/sh\n: > '{}'\nn=0\nwhile test ! -f '{}' && test \"$n\" -lt 500; do sleep 0.02; n=$((n+1)); done\n: > '{}'\nexit 7\n",
            ready.display(),
            release.display(),
            done.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&companion, fs::Permissions::from_mode(0o755)).unwrap();
    let mut child = Command::new(binary)
        .args(["spawn", "-M", "--rootfs", "/", "--", "/bin/true"])
        .env("PVISOR_RUN_HOME", temporary.path().join("jobs"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if !ready.exists() {
        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        panic!(
            "resolver did not start: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // The child is still ours and has not been reaped, so its PID cannot recycle.
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut finished = false;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            finished = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if !finished {
        let _ = child.kill();
    }
    fs::write(release, b"").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(finished, "cancellation waited for companion startup");
    assert_eq!(
        output.status.code(),
        Some(130),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn kernel_help_discovers_commands_and_default_execution_dispatches_spawn() {
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(!help.contains("\n  env "));
    assert!(help.contains("Run containers and agents in independent environments"));
    assert!(output.stderr.is_empty());
    assert!(!help.contains("--vm-"), "{help}");
    let headings = [
        "Commands:",
        "Agent Commands:",
        "Tools:",
        "Options:",
        "Examples:",
    ];
    let positions: Vec<_> = headings
        .iter()
        .map(|heading| {
            help.find(heading)
                .unwrap_or_else(|| panic!("missing {heading}: {help}"))
        })
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{help}");
    assert!(help.contains("\nCommands:"));
    assert!(!help.contains("\n  extensions "));
    for (start, end, names) in [
        (
            "Commands:\n",
            "Agent Commands:\n",
            &[
                "run", "build", "pull", "images", "rmi", "exec", "ps", "start", "stop", "rm",
                "logs",
            ][..],
        ),
        (
            "Agent Commands:\n",
            "Tools:\n",
            &[
                "spawn",
                "status",
                "kill",
                "review",
                "apply",
                "drop",
                "inspect",
                "checkpoint",
                "suspend",
                "resume",
                "fork",
            ][..],
        ),
        ("Tools:\n", "Options:\n", &["cache", "feature", "help"][..]),
    ] {
        let section = help.split(start).nth(1).unwrap().split(end).next().unwrap();
        for name in names {
            assert!(section.contains(&format!("  {name} ")), "{section}");
        }
        if start != "Tools:\n" {
            assert!(!section.contains("  tui "), "{section}");
        }
    }
    for name in ["service", "memory-pool"] {
        assert!(!help.contains(&format!("\n  {name} ")), "{help}");
    }
    for heading in [
        "Changes:",
        "Checkpoints:",
        "Jobs:",
        "Filesystems:",
        "Services:",
        "Trajectories:",
    ] {
        assert!(!help.contains(heading), "{help}");
    }
    for args in [vec!["--help"], vec!["-h"], vec!["help"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty(), "{:?}", output.stderr);
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(!text.contains("--vm-"), "{text}");
        for heading in headings {
            assert!(text.contains(heading), "{text}");
        }
    }

    for option in ["--gateway-stream-markdown", "--unknown-pvisor-option"] {
        let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .args(["spawn", option, "--", "/bin/true"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));
    }

    let help_command = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["help", "help"])
        .output()
        .unwrap();
    assert!(help_command.status.success());
    assert!(String::from_utf8_lossy(&help_command.stdout).contains("Usage: pvisor help"));

    for name in [
        "run", "spawn", "apply", "drop", "status", "kill", "fork", "inspect",
    ] {
        assert!(help.contains(&format!("  {name} ")), "{help}");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["--", "/bin/sh", "-c", "printf kernel-dispatched"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("kernel-dispatched"));
}

#[test]
fn retired_services_have_no_aliases_or_help_forwarding() {
    let temporary = tempfile::tempdir().unwrap();
    let kernel = temporary.path().join("pvisor");
    fs::copy(env!("CARGO_BIN_EXE_pvisor"), &kernel).unwrap();
    let marker = temporary.path().join("executed");
    for name in ["service", "memory-pool", "cluster", "worker"] {
        let companion = temporary.path().join(format!("pvisor-{name}"));
        fs::write(
            &companion,
            format!("#!/bin/sh\n: > '{}'\nexit 42\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(companion, fs::Permissions::from_mode(0o755)).unwrap();
    }
    // A PATH workload named service must not turn a retired command into a Job.
    let workload = temporary.path().join("service");
    fs::write(
        &workload,
        format!("#!/bin/sh\n: > '{}'\nexit 42\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(workload, fs::Permissions::from_mode(0o755)).unwrap();
    for args in [vec![], vec!["--help"], vec!["-h"], vec!["help"]] {
        let output = Command::new(&kernel).args(args).output().unwrap();
        assert!(output.status.success());
        let help = String::from_utf8_lossy(&output.stdout);
        assert!(!help.contains("Extensions:"), "{help}");
        for name in ["service", "memory-pool", "cluster", "worker"] {
            assert!(!help.contains(&format!("\n  {name} ")), "{help}");
        }
    }
    let mut retired = vec![
        vec!["service"],
        vec!["service", "--help"],
        vec!["service", "-h"],
        vec!["help", "service"],
    ];
    for name in [
        "run",
        "status",
        "restart",
        "stop",
        "node",
        "daemon",
        "cache",
        "memory-pool",
        "cluster",
        "worker",
    ] {
        retired.push(vec!["service", name]);
        retired.push(vec!["service", name, "--help"]);
        retired.push(vec!["help", "service", name, "submit"]);
    }
    for args in retired {
        for prefix in [
            vec![],
            vec!["--feature", "workload-aware-memory-offloading"],
        ] {
            let output = Command::new(&kernel)
                .args(prefix)
                .args(&args)
                .env("PATH", temporary.path())
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1), "{args:?}");
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("`pvisor service` has been retired")
            );
            assert!(output.stdout.is_empty(), "{args:?}");
        }
    }
    for name in ["memory-pool"] {
        for args in [vec!["help", name], vec![name, "--help"]] {
            let output = Command::new(&kernel)
                .args(&args)
                .env("PATH", temporary.path())
                .output()
                .unwrap();
            if args[0] == "help" {
                assert!(!output.status.success(), "{args:?}");
            }
            assert_ne!(output.status.code(), Some(42), "{args:?}");
        }
    }
    assert!(
        !marker.exists(),
        "a retired route executed a companion or workload"
    );
}

#[test]
fn explicit_default_execution_remains_available() {
    let temporary = tempfile::tempdir().unwrap();
    let kernel = temporary.path().join("pvisor");
    fs::copy(env!("CARGO_BIN_EXE_pvisor"), &kernel).unwrap();
    let output = Command::new(&kernel)
        .args(["--", "/bin/sh", "-c", "printf explicit-workload"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("explicit-workload"));
}

#[test]
fn cli_has_three_binaries_and_integrates_cache() {
    let manifest: toml::Value = toml::from_str(include_str!("../Cargo.toml")).unwrap();
    let bins: Vec<_> = manifest["bin"]
        .as_array()
        .unwrap()
        .iter()
        .map(|bin| bin["name"].as_str().unwrap())
        .collect();
    assert_eq!(bins, ["pvisor", "pvisor-tui", "pvisor-replay"]);
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .arg("cache")
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("Usage: pvisor cache"), "{help}");
    assert!(!help.contains("  serve"), "{help}");
    assert!(!help.contains("pvisor service"), "{help}");
}

#[test]
fn isolated_core_keeps_job_commands_and_runs_without_extensions() {
    let temporary = tempfile::tempdir().unwrap();
    let kernel = temporary.path().join("pvisor");
    fs::copy(env!("CARGO_BIN_EXE_pvisor"), &kernel).unwrap();
    let output = Command::new(&kernel)
        .env("PATH", temporary.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    for name in [
        "run", "build", "spawn", "status", "kill", "pull", "images", "rmi", "exec", "ps", "start",
        "stop", "rm", "logs", "inspect", "fork", "apply", "drop", "cache", "help",
    ] {
        assert!(help.contains(&format!("\n  {name} ")), "{help}");
    }
    for name in ["env", "service", "daemon", "memory-pool", "tui", "replay"] {
        assert!(!help.contains(&format!("\n  {name} ")), "{help}");
    }
    let output = Command::new(&kernel)
        .args(["/bin/sh", "-c", "printf standalone-kernel"])
        .env("PATH", temporary.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("standalone-kernel"));
    let marker = temporary.path().join("shadowed");
    let plugin = temporary.path().join("pvisor-status");
    fs::write(
        &plugin,
        format!("#!/bin/sh\n: > '{}'\nexit 42\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&plugin, fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(&kernel)
        .args(["status", "--help"])
        .env("PATH", temporary.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!marker.exists(), "extension shadowed a core command");
}

#[test]
fn path_cannot_supply_companions_and_unknown_commands_are_not_discovered() {
    let installation = tempfile::tempdir().unwrap();
    let untrusted = tempfile::tempdir().unwrap();
    let kernel = installation.path().join("pvisor");
    fs::copy(env!("CARGO_BIN_EXE_pvisor"), &kernel).unwrap();
    for name in ["daemon", "tui", "probe"] {
        let path = untrusted.path().join(format!("pvisor-{name}"));
        fs::write(&path, "#!/bin/sh\nexit 42\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let output = Command::new(&kernel)
            .args(["help", name])
            .env("PATH", untrusted.path())
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        if name == "tui" || name == "daemon" {
            assert!(error.contains("companion is not installed"), "{error}");
        } else {
            assert!(!error.contains("was removed"), "{error}");
        }
    }
    fs::copy(
        untrusted.path().join("pvisor-probe"),
        installation.path().join("pvisor-probe"),
    )
    .unwrap();
    let output = Command::new(&kernel).arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    for name in ["tui", "probe", "extensions"] {
        assert!(!help.contains(&format!("\n  {name} ")), "{help}");
    }
}

#[test]
fn unregistered_names_follow_default_execution_without_command_aliases() {
    let temporary = tempfile::tempdir().unwrap();
    let kernel = temporary.path().join("pvisor");
    fs::copy(env!("CARGO_BIN_EXE_pvisor"), &kernel).unwrap();
    for name in [
        "docker",
        "snapshot",
        "extensions",
        "cluster",
        "worker",
        "memory-pool",
        "env",
        "ir",
        "trace",
        "job",
    ] {
        let workload = temporary.path().join(name);
        fs::write(&workload, "#!/bin/sh\nprintf '%s\\n' \"$@\"\nexit 42\n").unwrap();
        fs::set_permissions(&workload, fs::Permissions::from_mode(0o755)).unwrap();
        let output = Command::new(&kernel)
            .args([name, "space argument", "--literal", ""])
            .env("PATH", temporary.path())
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(42),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"space argument\n--literal\n\n", "{name}");
    }
}
