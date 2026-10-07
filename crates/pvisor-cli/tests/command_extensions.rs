//! First-party discovery is inert; dispatch preserves argv and exit.
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

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
fn kernel_help_discovers_commands_and_default_execution_dispatches_run() {
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(!help.contains("\n  env "));
    assert!(help.contains("Run agents, review changes, and manage Jobs"));
    let headings = [
        "Execution:",
        "Changes:",
        "Checkpoints:",
        "Tools:",
        "Options:",
    ];
    let positions: Vec<_> = headings
        .iter()
        .map(|heading| {
            help.find(heading)
                .unwrap_or_else(|| panic!("missing {heading}: {help}"))
        })
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{help}");
    assert!(!help.contains("\nCommands:"));
    assert!(!help.contains("\n  extensions "));
    for (start, end, names) in [
        ("Execution:\n", "Changes:\n", &["run", "status", "kill"][..]),
        (
            "Changes:\n",
            "Checkpoints:\n",
            &["inspect", "review", "apply", "drop"][..],
        ),
        (
            "Checkpoints:\n",
            "Tools:\n",
            &["checkpoint", "suspend", "resume", "fork"][..],
        ),
    ] {
        let section = help.split(start).nth(1).unwrap().split(end).next().unwrap();
        for name in names {
            assert!(section.contains(&format!("  {name} ")), "{section}");
        }
        assert!(!section.contains("  tui "), "{section}");
    }
    for name in ["service", "daemon", "cache", "memory-pool"] {
        assert!(!help.contains(&format!("\n  {name} ")), "{help}");
    }
    for heading in ["Jobs:", "Filesystems:", "Services:", "Trajectories:"] {
        assert!(!help.contains(heading), "{help}");
    }
    for args in [vec!["--help"], vec!["-h"], vec!["help"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8_lossy(&output.stdout);
        for heading in headings {
            assert!(text.contains(heading), "{text}");
        }
    }

    for option in ["--gateway-stream-markdown", "--unknown-pvisor-option"] {
        let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .args(["run", option, "--", "/bin/true"])
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

    for name in ["run", "apply", "drop", "status", "kill", "fork", "inspect"] {
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
    for name in [
        "service",
        "daemon",
        "cache",
        "memory-pool",
        "cluster",
        "worker",
    ] {
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
        for name in [
            "service",
            "daemon",
            "cache",
            "memory-pool",
            "cluster",
            "worker",
        ] {
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
    for name in ["daemon", "cache", "memory-pool"] {
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
fn cli_has_four_binaries_and_preserves_standalone_cache() {
    let manifest: toml::Value = toml::from_str(include_str!("../Cargo.toml")).unwrap();
    let bins: Vec<_> = manifest["bin"]
        .as_array()
        .unwrap()
        .iter()
        .map(|bin| bin["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        bins,
        ["pvisor", "pvisor-cache", "pvisor-tui", "pvisor-replay"]
    );
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor-cache"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("Usage: pvisor-cache"), "{help}");
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
        "run", "status", "kill", "inspect", "fork", "apply", "drop", "help",
    ] {
        assert!(help.contains(&format!("\n  {name} ")), "{help}");
    }
    for name in [
        "env",
        "service",
        "daemon",
        "cache",
        "memory-pool",
        "tui",
        "replay",
    ] {
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
    for name in ["tui", "probe"] {
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
        if name == "tui" {
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
        "snapshot",
        "extensions",
        "cluster",
        "worker",
        "cache",
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
