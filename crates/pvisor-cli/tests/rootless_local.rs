#![cfg(target_os = "linux")]

use pvisor::RunBundle;
use pvisor::sandbox::SANDBOX_SETUP_EXIT_CODE;
use pvisor_core::{IsolationKind, RunFailureKind};
use std::fs;
use std::fs::OpenOptions;
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn only_run(root: &Path) -> PathBuf {
    if root.join("run-bundle.json").is_file() {
        return root.to_path_buf();
    }
    let runs = fs::read_dir(root)
        .expect("read Run root")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("run-bundle.json").is_file())
        .collect::<Vec<_>>();
    assert_eq!(runs.len(), 1, "expected one finalized Run in {root:?}");
    runs.into_iter().next().unwrap()
}

fn stage_root(run_home: &Path) -> PathBuf {
    run_home
        .parent()
        .expect("temporary run root has a parent")
        .join("stage")
}

fn setup_failure(root: &Path) -> Option<String> {
    let root = if root.exists() {
        root.to_path_buf()
    } else {
        stage_root(root)
    };
    if root.join("run-bundle.json").is_file() {
        return RunBundle::read(&root).ok()?.run.output.stderr;
    }
    let run = fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.join("run-bundle.json").is_file())?;
    let bundle = RunBundle::read(&run).ok()?;
    bundle.run.output.stderr
}

fn user_namespaces_are_unavailable(stderr: &str) -> bool {
    const CONTEXTS: &[&str] = &[
        "initialize rootless namespaces: ",
        "initialize rootless user and mount namespaces: ",
    ];
    stderr.lines().any(|line| {
        CONTEXTS.iter().any(|context| {
            let Some((_, error)) = line.split_once(context) else {
                return false;
            };
            error == "unshare user namespace: Operation not permitted (os error 1)"
                || error == "unshare user namespace: Permission denied (os error 13)"
        })
    })
}

fn skip_if_user_namespaces_are_explicitly_optional(
    run_home: &Path,
    output: &std::process::Output,
) -> bool {
    if std::env::var_os("PVISOR_TEST_ALLOW_NO_USERNS").is_none() || output.status.success() {
        return false;
    }
    // Namespace setup can fail before a Run Bundle exists. In the optional CI
    // shard, verify host capability directly before inspecting bundle details.
    if skip_if_rootless_runtime_is_explicitly_optional() {
        return true;
    }
    let combined = String::from_utf8_lossy(&output.stderr);
    // Safe-best-effort intentionally falls back to the host process when the
    // runner cannot create namespaces. Treat that capability result as a
    // skippable environment condition, just like the launcher setup error.
    if combined.contains("falling back to host process") {
        eprintln!("skipping: the test host disables required namespaces");
        return true;
    }
    let Some(stderr) = setup_failure(run_home) else {
        return false;
    };
    if !user_namespaces_are_unavailable(&stderr) {
        if !stderr.is_empty() {
            eprintln!("rootless sandbox failure was not skippable: {stderr}");
        }
        return false;
    }
    eprintln!("skipping: the test host disables unprivileged user namespaces: {stderr}");
    true
}

fn skip_if_rootless_runtime_is_explicitly_optional() -> bool {
    if std::env::var_os("PVISOR_TEST_ALLOW_NO_USERNS").is_none() {
        return false;
    }
    let available = Command::new("unshare")
        .args(["--user", "--mount", "--pid", "--fork", "true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !available {
        eprintln!("skipping: the test host cannot create rootless user/mount/PID namespaces");
        return true;
    }
    if OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fuse")
        .is_err()
    {
        eprintln!("skipping: the test host cannot open /dev/fuse");
        return true;
    }
    false
}

#[test]
fn ordinary_shell_writes_lower_and_safe_shell_stages_home_and_workspace() {
    if skip_if_rootless_runtime_is_explicitly_optional() {
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("home");
    let workspace = temporary.path().join("workspace");
    fs::create_dir_all(home.join(".codex")).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    fs::write(home.join(".codex/state"), b"lower").unwrap();

    let normal = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("HOME", &home)
        .env_remove("CODEX_HOME")
        .current_dir(&workspace)
        .args([
            "spawn",
            "--",
            "/bin/sh",
            "-c",
            "printf direct > \"$HOME/.codex/state\"; printf direct > project-state",
        ])
        .output()
        .unwrap();
    assert!(
        normal.status.success(),
        "normal run: {}",
        String::from_utf8_lossy(&normal.stderr)
    );
    assert_eq!(fs::read(home.join(".codex/state")).unwrap(), b"direct");
    assert_eq!(
        fs::read(workspace.join("project-state")).unwrap(),
        b"direct"
    );

    fs::write(home.join(".codex/state"), b"lower").unwrap();
    fs::remove_file(workspace.join("project-state")).unwrap();
    let safe = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("HOME", &home)
        .env_remove("CODEX_HOME")
        .current_dir(&workspace)
        .args(["spawn", "--safe", "--stage"])
        .arg(temporary.path().join("stage"))
        .args([
            "--",
            "/bin/sh",
            "-c",
            "test \"$(pwd -P)\" = \"$PVISOR_OVERLAY_TARGET\"; printf staged > \"$HOME/.codex/state\"; printf staged > project-state; cd \"$PVISOR_OVERLAY_TARGET\"; printf staged > alias-state",
        ])
        .output()
        .unwrap();
    assert!(
        safe.status.success(),
        "safe run: {}",
        String::from_utf8_lossy(&safe.stderr)
    );
    assert_eq!(fs::read(home.join(".codex/state")).unwrap(), b"lower");
    assert!(!workspace.join("project-state").exists());
    assert!(!workspace.join("alias-state").exists());
    let bundle = RunBundle::read(&temporary.path().join("stage")).unwrap();
    let filesystem = bundle.filesystem.expect("safe workspace stage");
    assert!(filesystem.upper.join("project-state").is_file());
    assert!(filesystem.upper.join("alias-state").is_file());
}

#[test]
fn safe_shell_supports_nested_stages_without_exposing_their_backing() {
    if skip_if_rootless_runtime_is_explicitly_optional() {
        return;
    }
    for (name, nested_stage, nested_home) in [
        ("external", false, false),
        ("nested", true, false),
        ("nested-home", true, true),
    ] {
        // Keep every fixture under the checkout, independent of the runner's
        // TMPDIR. A synthetic-root Session gives /tmp its own private tmpfs.
        let temporary = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let workspace = temporary.path().join("workspace");
        let home = if nested_home {
            workspace.join("home")
        } else {
            temporary.path().join("home")
        };
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("state"), b"lower").unwrap();
        let stage = if nested_stage {
            workspace.join("experiment")
        } else {
            temporary.path().join("experiment")
        };
        let command = || {
            let mut command = Command::new("timeout");
            command
                .args(["--kill-after=2s", "10s", env!("CARGO_BIN_EXE_pvisor")])
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &home)
                .env("TMPDIR", temporary.path())
                .env("PVISOR_RUN_HOME", temporary.path().join("runs"))
                .current_dir(&workspace)
                .stdin(Stdio::null());
            command
        };
        let output = command()
            .args([
                "spawn",
                "--safe",
                "--overlaynet-deny-all",
                "--stdio",
                "capture",
                "--stage",
            ])
            .arg(if nested_stage {
                Path::new("./experiment")
            } else {
                &stage
            })
            .args([
                "--",
                "/bin/sh",
                "-exc",
                r#"
stage=$1
test "$PVISOR_OVERLAY_STAGE" = "$stage"
test ! -e "$stage/run.json"
test ! -e "$stage/overlay.json"
test ! -e "$PVISOR_OVERLAY_UPPER"
if (cat "$stage/run.json") 2>/dev/null; then exit 1; fi
for directory in "$stage" "$PVISOR_OVERLAY_UPPER"; do
    if (printf forbidden > "$directory/management-probe") 2>/dev/null; then
        echo "management directory writable: $directory" >&2
        exit 1
    fi
done
alias_written=no
if test "$2" = nested; then
    merged="$stage/.overlay-mounts/$PVISOR_OVERLAY_ID/merged"
    test ! -e ./experiment
    test ! -e "$stage"
    test ! -e "$merged"
    if (printf forbidden > "$merged/backing-probe") 2>/dev/null; then exit 1; fi
else
    merged="$stage/merged"
    # If reachable, this is a COW workspace alias, not the physical upper.
    # Its absolute path need not remain available behind a private /tmp.
    if test -e "$merged"; then
        test -d "$merged"
        printf alias > "$merged/alias-result.txt"
        alias_written=yes
    elif (printf forbidden > "$merged/backing-probe") 2>/dev/null; then
        echo "hidden merged path writable: $merged" >&2
        exit 1
    fi
fi
printf '%s\n%s\n' "$merged" "$alias_written"
printf staged > result.txt
printf private > "$HOME/state"
"#,
                "stage-visibility-probe",
            ])
            .arg(&stage)
            .arg(if nested_stage { "nested" } else { "external" })
            .output()
            .unwrap();
        let bundle = RunBundle::read(&stage);
        assert!(
            output.status.success(),
            "{name}: stdout: {}\nstderr: {}\ncaptured workload: {:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            bundle.as_ref().map(|bundle| &bundle.run.output)
        );
        let bundle = bundle.unwrap();
        assert!(bundle.run.failure.is_none(), "{name}: {:?}", bundle.run);
        assert!(bundle.safety.filesystem_non_bypassable, "{name}");
        assert!(bundle.safety.network_non_bypassable, "{name}");
        assert_eq!(fs::read(home.join("state")).unwrap(), b"lower", "{name}");
        assert!(!workspace.join("result.txt").exists(), "{name}");
        assert!(!workspace.join("alias-result.txt").exists(), "{name}");
        assert!(!stage.join("management-probe").exists(), "{name}");
        let overlay: serde_json::Value =
            serde_json::from_slice(&fs::read(stage.join("overlay.json")).unwrap()).unwrap();
        let stdout = bundle.run.output.stdout.as_deref().unwrap();
        let mut lines = stdout.lines();
        assert_eq!(
            lines.next(),
            overlay["merged_dir"].as_str(),
            "{name}: probe must address the actual recorded merged backing"
        );
        let alias_written = lines.next().unwrap();
        assert!(matches!(alias_written, "yes" | "no"), "{name}: {stdout}");
        assert!(lines.next().is_none(), "{name}: {stdout}");
        let upper = &bundle.filesystem.as_ref().unwrap().upper;
        assert!(!upper.join("management-probe").exists(), "{name}");
        assert!(!upper.join("backing-probe").exists(), "{name}");
        assert_eq!(fs::read(upper.join("result.txt")).unwrap(), b"staged");
        if alias_written == "yes" {
            assert!(!nested_stage, "{name}: hidden backing was writable");
            assert_eq!(fs::read(upper.join("alias-result.txt")).unwrap(), b"alias");
        } else {
            assert!(!upper.join("alias-result.txt").exists(), "{name}");
        }
        let review = command()
            .arg("review")
            .arg(&stage)
            .arg("--json")
            .output()
            .unwrap();
        assert!(review.status.success(), "{name}: {review:?}");
        assert!(String::from_utf8_lossy(&review.stdout).contains("result.txt"));
        let dropped = command().arg("drop").arg(&stage).output().unwrap();
        assert!(dropped.status.success(), "{name}: {dropped:?}");
        assert!(!upper.join("result.txt").exists(), "{name}");
        assert!(!upper.join("alias-result.txt").exists(), "{name}");
        assert!(
            !fs::read_to_string("/proc/self/mountinfo")
                .unwrap()
                .contains(temporary.path().to_str().unwrap())
        );
        eprintln!(
            "{name}: alias_written={alias_written}, sandbox enforced, management protected, lower unchanged, review/drop succeeded"
        );
    }
}

#[test]
fn safe_abnormal_exits_retain_reviewable_stages_and_release_owners_before_retry() {
    if skip_if_rootless_runtime_is_explicitly_optional() {
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    let home = temporary.path().join("home");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&home).unwrap();

    // Bound every real CLI request, including its owner, without signalling any
    // process discovered by PID. Each spawn completes before the next begins.
    let command = || {
        let mut command = Command::new("timeout");
        command
            .args(["--kill-after=2s", "25s", env!("CARGO_BIN_EXE_pvisor")])
            .current_dir(&workspace)
            .env("HOME", &home)
            .env("TMPDIR", temporary.path())
            .env("PVISOR_RUN_HOME", &run_home)
            .stdin(Stdio::null());
        command
    };
    for (name, script, code, state) in [
        (
            "nonzero",
            "printf retained > result.txt; exit 23",
            23,
            "failed",
        ),
        (
            "signal",
            "printf retained > result.txt; kill -TERM $$",
            143,
            "failed",
        ),
        ("healthy", "printf retained > result.txt", 0, "completed"),
    ] {
        let stage = temporary.path().join(name);
        let started = std::time::Instant::now();
        let output = command()
            .args([
                "spawn",
                "--no-agent-defaults",
                "--safe",
                "--overlaynet-deny-all",
                "--stdio",
                "capture",
                "--stage",
            ])
            .arg(&stage)
            .args(["--", "/bin/sh", "-c", script])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(code),
            "{name}: stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let bundle = RunBundle::read(&stage).unwrap();
        assert_eq!(bundle.run.exit_code, Some(code));
        assert!(bundle.safety.safe_profile_requested);
        assert!(bundle.safety.filesystem_non_bypassable);
        assert!(bundle.safety.filesystem_changes_staged);
        if code != 0 {
            assert_eq!(
                bundle.run.failure.as_ref().unwrap().kind,
                RunFailureKind::ProcessExit
            );
        } else {
            assert!(bundle.run.failure.is_none());
        }
        assert!(!workspace.join("result.txt").exists());
        let upper = &bundle.filesystem.as_ref().unwrap().upper;
        assert_eq!(fs::read(upper.join("result.txt")).unwrap(), b"retained");

        let status = command()
            .arg("status")
            .arg(&stage)
            .arg("--json")
            .output()
            .unwrap();
        assert!(status.status.success(), "{status:?}");
        let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(status["live"], false);
        assert_eq!(status["state"], state);
        assert_eq!(status["run"]["state"], state);
        assert_eq!(status["run"]["overlay"]["state"], "staged");
        let owner = status["run"]["pid"].as_u64().unwrap();
        assert!(owner > 0);
        assert!(!Path::new(&format!("/proc/{owner}")).exists());
        let mounts = fs::read_to_string("/proc/self/mountinfo").unwrap();
        assert!(
            !mounts.contains(temporary.path().to_str().unwrap()),
            "{name}: fixture mount survived completion"
        );
        let review = command()
            .arg("review")
            .arg(&stage)
            .arg("--json")
            .output()
            .unwrap();
        assert!(review.status.success(), "{review:?}");
        assert!(String::from_utf8_lossy(&review.stdout).contains("result.txt"));
        let drop = command().arg("drop").arg(&stage).output().unwrap();
        assert!(drop.status.success(), "{drop:?}");
        assert!(!upper.join("result.txt").exists());
        assert!(!workspace.join("result.txt").exists());
        let dropped = command()
            .arg("status")
            .arg(&stage)
            .arg("--json")
            .output()
            .unwrap();
        assert!(dropped.status.success(), "{dropped:?}");
        let dropped: serde_json::Value = serde_json::from_slice(&dropped.stdout).unwrap();
        assert_eq!(dropped["run"]["overlay"]["state"], "discarded");
        assert_eq!(dropped["filesystem"]["changed_files"], 0);
        eprintln!(
            "{name}: exit={code}, state={state}, owner={owner} gone, no fixture mounts, review/drop succeeded ({:?})",
            started.elapsed()
        );
    }
}

#[test]
fn safe_local_executable_cannot_escape_the_workspace() {
    if skip_if_rootless_runtime_is_explicitly_optional() {
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let outside = temporary.path().join("outside");
    let run_home = temporary.path().join("runs");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"host-secret").unwrap();
    symlink(&outside, workspace.join("escape-link")).unwrap();

    let script = r#"
set -eu
printf 'staged' > staged.txt
if cat "$OUTSIDE_SECRET" >/dev/null 2>&1; then
  echo 'outside read unexpectedly succeeded' >&2
  exit 40
fi
if printf 'escaped' > "$OUTSIDE_WRITE" 2>/dev/null; then
  echo 'outside write unexpectedly succeeded' >&2
  exit 41
fi
if cat escape-link/secret.txt >/dev/null 2>&1; then
  echo 'symlink escape read unexpectedly succeeded' >&2
  exit 42
fi
if printf 'escaped' > escape-link/symlink-escaped.txt 2>/dev/null; then
  echo 'symlink escape write unexpectedly succeeded' >&2
  exit 43
fi
if cat "/proc/self/root$OUTSIDE_SECRET" >/dev/null 2>&1; then
  echo 'proc root escape unexpectedly succeeded' >&2
  exit 44
fi
printf '%s:%s:%s\n' "$PVISOR_SANDBOX_FILESYSTEM" "$PVISOR_SANDBOX_LANDLOCK_ABI" "$PVISOR_SANDBOX_USER_NAMESPACE"
"#;
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_RUN_HOME", &run_home)
        .env("OUTSIDE_SECRET", outside.join("secret.txt"))
        .env("OUTSIDE_WRITE", outside.join("escaped.txt"))
        .args([
            "spawn",
            "--safe",
            "--stdio",
            "capture",
            "--stage",
            temporary.path().join("stage").to_str().unwrap(),
            "--filesystem",
            "sandbox",
            "--pass-env",
            "OUTSIDE_SECRET",
            "--pass-env",
            "OUTSIDE_WRITE",
        ])
        .current_dir(&workspace)
        .args(["--", "/bin/sh", "-c", script])
        .output()
        .unwrap();

    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!outside.join("escaped.txt").exists());
    assert!(!outside.join("symlink-escaped.txt").exists());
    assert!(!workspace.join("staged.txt").exists());

    let run = only_run(&stage_root(&run_home));
    let bundle = RunBundle::read(&run).unwrap();
    assert_eq!(
        bundle
            .run
            .executor
            .as_ref()
            .map(|executor| executor.isolation),
        Some(IsolationKind::RootlessProcess)
    );
    assert!(bundle.safety.filesystem_non_bypassable);
    assert!(bundle.safety.filesystem_changes_staged);
    assert!(!bundle.safety.network_non_bypassable);
    assert!(
        bundle
            .safety
            .warnings
            .iter()
            .any(|warning| warning.contains("process-tree cleanup"))
    );
    assert!(
        bundle
            .safety
            .warnings
            .iter()
            .all(|warning| !warning.contains("host PID namespace"))
    );
    let filesystem = bundle
        .filesystem
        .as_ref()
        .expect("staged filesystem summary");
    assert!(filesystem.upper.join("staged.txt").is_file());
    assert!(filesystem.changed_files >= 1);
    assert!(
        bundle
            .run
            .output
            .stdout
            .as_deref()
            .is_some_and(|stdout| stdout.contains("landlock:") && stdout.ends_with(":1\n")),
        "captured output: {:?}",
        bundle.run.output.stdout
    );
}

#[test]
fn safe_local_executable_cannot_mutate_outside_metadata() {
    if skip_if_rootless_runtime_is_explicitly_optional() {
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let outside = temporary.path().join("outside");
    let run_home = temporary.path().join("runs");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&outside).unwrap();
    let protected = outside.join("protected.txt");
    fs::write(&protected, b"host-content").unwrap();
    fs::set_permissions(&protected, fs::Permissions::from_mode(0o640)).unwrap();

    let script = r#"
set -eu
if chmod 000 "$OUTSIDE_FILE" 2>/dev/null; then exit 60; fi
if rm "$OUTSIDE_FILE" 2>/dev/null; then exit 61; fi
if mv "$OUTSIDE_FILE" stolen.txt 2>/dev/null; then exit 62; fi
if ln "$OUTSIDE_FILE" hardlink.txt 2>/dev/null; then exit 63; fi
if mkdir "$OUTSIDE_DIR/created" 2>/dev/null; then exit 64; fi
if touch -r "$OUTSIDE_FILE" timestamp-copy 2>/dev/null; then exit 65; fi
printf metadata-denied
"#;
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_RUN_HOME", &run_home)
        .env("OUTSIDE_FILE", &protected)
        .env("OUTSIDE_DIR", &outside)
        .args([
            "spawn",
            "--safe",
            "--stdio",
            "capture",
            "--stage",
            temporary.path().join("stage").to_str().unwrap(),
            "--filesystem",
            "sandbox",
            "--pass-env",
            "OUTSIDE_FILE",
            "--pass-env",
            "OUTSIDE_DIR",
        ])
        .current_dir(&workspace)
        .args(["--", "/bin/sh", "-c", script])
        .output()
        .unwrap();

    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&protected).unwrap(), b"host-content");
    assert_eq!(
        fs::metadata(&protected).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert!(!outside.join("created").exists());
    assert!(!workspace.join("stolen.txt").exists());
    assert!(!workspace.join("hardlink.txt").exists());
    assert!(!workspace.join("timestamp-copy").exists());

    let bundle = RunBundle::read(&only_run(&stage_root(&run_home))).unwrap();
    assert!(bundle.safety.filesystem_read_non_bypassable);
    assert!(bundle.safety.filesystem_write_non_bypassable);
    assert!(
        bundle
            .run
            .output
            .stdout
            .as_deref()
            .is_some_and(|stdout| stdout == "metadata-denied")
    );
}

#[test]
fn safe_run_selectively_applies_then_drops_remaining_changes() {
    if skip_if_rootless_runtime_is_explicitly_optional() {
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::create_dir_all(workspace.join("tests")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_RUN_HOME", &run_home)
        .args(["spawn", "--safe", "--stdio", "capture", "--stage"])
        .arg(temporary.path().join("stage"))
        .current_dir(&workspace)
        .args([
            "--",
            "/bin/sh",
            "-c",
            "printf promoted > src/promote.txt; printf deferred > tests/defer.txt",
        ])
        .output()
        .unwrap();

    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!workspace.join("src/promote.txt").exists());
    assert!(!workspace.join("tests/defer.txt").exists());

    let run = only_run(&stage_root(&run_home));
    let apply = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_RUN_HOME", &run_home)
        .args(["apply"])
        .arg(&run)
        .args(["--path", "src"])
        .output()
        .unwrap();
    assert!(
        apply.status.success(),
        "selective apply failed: {}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert_eq!(
        fs::read_to_string(workspace.join("src/promote.txt")).unwrap(),
        "promoted"
    );
    assert!(!workspace.join("tests/defer.txt").exists());

    let status = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_RUN_HOME", &run_home)
        .args(["status"])
        .arg(&run)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["apply_batch_count"].as_u64().unwrap(), 1);
    assert_eq!(status["schema_version"], 3);
    assert_eq!(status["run"]["overlay"]["state"], "staged");
    assert!(
        status["filesystem"]["sample_paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path.as_str().is_some_and(|path| path.contains("defer.txt")))
    );

    let drop = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_RUN_HOME", &run_home)
        .arg("drop")
        .arg(&run)
        .output()
        .unwrap();
    assert!(
        drop.status.success(),
        "drop failed: {}",
        String::from_utf8_lossy(&drop.stderr)
    );
    assert!(!workspace.join("tests/defer.txt").exists());

    let status = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_RUN_HOME", &run_home)
        .args(["status"])
        .arg(&run)
        .arg("--json")
        .output()
        .unwrap();
    assert!(status.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["schema_version"], 3);
    assert_eq!(status["run"]["overlay"]["state"], "discarded");
    assert_eq!(status["filesystem"]["changed_files"], 0);
    assert_eq!(status["apply_batch_count"].as_u64().unwrap(), 1);
}

#[test]
fn safe_apply_refuses_to_overwrite_a_concurrently_changed_target() {
    if skip_if_rootless_runtime_is_explicitly_optional() {
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("value.txt"), b"original").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_RUN_HOME", &run_home)
        .args([
            "spawn",
            "--stdio",
            "capture",
            "--filesystem",
            "sandbox",
            "--stage",
        ])
        .arg(temporary.path().join("stage"))
        .current_dir(&workspace)
        .args(["--", "/bin/sh", "-c", "printf staged > value.txt"])
        .output()
        .unwrap();
    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }
    assert!(
        output.status.success(),
        "safe run failed ({}):\nstdout:\n{}\nstderr:\n{}\nsetup failure: {:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        setup_failure(&run_home)
    );
    assert_eq!(fs::read(workspace.join("value.txt")).unwrap(), b"original");
    fs::write(workspace.join("value.txt"), b"concurrent").unwrap();

    let run = only_run(&stage_root(&run_home));
    let apply = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_RUN_HOME", &run_home)
        .arg("apply")
        .arg(&run)
        .arg("--all")
        .output()
        .unwrap();
    assert!(!apply.status.success());
    assert!(
        String::from_utf8_lossy(&apply.stderr).contains("target changed after staging"),
        "stderr: {}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert_eq!(
        fs::read(workspace.join("value.txt")).unwrap(),
        b"concurrent"
    );
    let bundle = RunBundle::read(&run).unwrap();
    assert_eq!(
        fs::read(bundle.filesystem.as_ref().unwrap().upper.join("value.txt")).unwrap(),
        b"staged"
    );
}

#[test]
fn safe_launcher_closes_inherited_host_file_descriptors() {
    if skip_if_rootless_runtime_is_explicitly_optional() {
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    let secret_path = temporary.path().join("descriptor-secret.txt");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(&secret_path, b"descriptor-secret").unwrap();

    let secret = OpenOptions::new().read(true).open(&secret_path).unwrap();
    let leaked_fd = secret.as_raw_fd();
    let flags = unsafe { libc::fcntl(leaked_fd, libc::F_GETFD) };
    assert!(flags >= 0, "read descriptor flags");
    let cleared = unsafe { libc::fcntl(leaked_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) };
    assert_eq!(cleared, 0, "make the fixture descriptor inheritable");

    // An already-connected socket would bypass pathname and connect-time
    // policy if it survived exec, so exercise it independently from the file.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let inherited_socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let socket_fd = inherited_socket.as_raw_fd();
    let socket_flags = unsafe { libc::fcntl(socket_fd, libc::F_GETFD) };
    assert!(socket_flags >= 0, "read socket descriptor flags");
    let cleared =
        unsafe { libc::fcntl(socket_fd, libc::F_SETFD, socket_flags & !libc::FD_CLOEXEC) };
    assert_eq!(cleared, 0, "make the fixture socket inheritable");

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_RUN_HOME", &run_home)
        .env("PVISOR_LEAKED_FD", leaked_fd.to_string())
        .env("PVISOR_LEAKED_SOCKET_FD", socket_fd.to_string())
        .args([
            "spawn",
            "--safe",
            "--stdio",
            "capture",
            "--stage",
            temporary.path().join("stage").to_str().unwrap(),
            "--filesystem",
            "sandbox",
            "--overlaynet-deny-all",
            "--pass-env",
            "PVISOR_LEAKED_FD",
            "--pass-env",
            "PVISOR_LEAKED_SOCKET_FD",
        ])
        .current_dir(&workspace)
        .arg("--")
        .arg(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "inherited_fd_probe_agent",
            "--nocapture",
        ])
        .output()
        .unwrap();

    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}\nsandbox stderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        setup_failure(&stage_root(&run_home)).unwrap_or_default()
    );
    let bundle = RunBundle::read(&only_run(&stage_root(&run_home))).unwrap();
    assert!(bundle.safety.filesystem_non_bypassable);
    assert!(
        bundle
            .run
            .output
            .stdout
            .as_deref()
            .is_some_and(|stdout| stdout.contains("inherited descriptors closed"))
    );
}

/// Re-enter this test binary as the untrusted Agent. The parent deliberately
/// passes an inheritable descriptor through pVisor; the trusted launcher must
/// close it before executing this function.
#[test]
#[ignore]
fn inherited_fd_probe_agent() {
    for name in ["PVISOR_LEAKED_FD", "PVISOR_LEAKED_SOCKET_FD"] {
        let fd = std::env::var(name)
            .unwrap_or_else(|_| panic!("missing {name}"))
            .parse::<libc::c_int>()
            .expect("numeric descriptor");
        let result = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_eq!(result, -1, "host descriptor {fd} remained open");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EBADF),
            "closed descriptor should report EBADF"
        );
        assert!(
            !PathBuf::from(format!("/proc/self/fd/{fd}")).exists(),
            "closed descriptor remained reachable through procfs"
        );
    }
    println!("inherited descriptors closed");
}

#[test]
fn denied_network_uses_a_private_network_namespace() {
    if skip_if_rootless_runtime_is_explicitly_optional() {
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    let host_write = temporary.path().join("host-visible.txt");
    fs::create_dir_all(&workspace).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let host_port = listener.local_addr().unwrap().port().to_string();

    let script = r#"
set -eu
test "$PVISOR_SANDBOX_NETWORK" = deny
test "$PVISOR_SANDBOX_FILESYSTEM" = host
printf 'host-visible' > "$HOST_WRITE"
if exec 3<>"/dev/tcp/127.0.0.1/${HOST_PORT}"; then
  echo 'host listener unexpectedly reachable' >&2
  exit 50
fi
printf 'network:%s\n' "$PVISOR_SANDBOX_NETWORK"
"#;
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_RUN_HOME", &run_home)
        .env("HOST_PORT", host_port)
        .args([
            "spawn",
            "--stdio",
            "capture",
            "--stage",
            temporary.path().join("stage").to_str().unwrap(),
            "--filesystem",
            "host",
            "--overlaynet-deny-all",
            "--pass-env",
            "HOST_PORT",
            "--pass-env",
            "HOST_WRITE",
        ])
        .current_dir(&workspace)
        .env("HOST_WRITE", &host_write)
        .args(["--", "/bin/bash", "-c", script])
        .output()
        .unwrap();

    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(&host_write).unwrap(), "host-visible");

    let run = only_run(&stage_root(&run_home));
    let bundle = RunBundle::read(&run).unwrap();
    assert!(bundle.safety.network_non_bypassable);
    assert!(
        bundle
            .run
            .output
            .stdout
            .as_deref()
            .is_some_and(|stdout| stdout.contains("network:deny")),
        "captured output: {:?}",
        bundle.run.output.stdout
    );
    assert!(
        bundle
            .safety
            .warnings
            .iter()
            .all(|warning| !warning.contains("direct sockets may bypass")),
        "safety warnings: {:?}",
        bundle.safety.warnings
    );
}

#[test]
fn synthetic_root_hides_ungranted_host_unix_sockets() {
    if skip_if_rootless_runtime_is_explicitly_optional() {
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    fs::create_dir_all(&workspace).unwrap();
    let host_socket = temporary.path().join("host-control.sock");
    let _listener = UnixListener::bind(&host_socket).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_RUN_HOME", &run_home)
        .env("PVISOR_SOCKET_PROBE", &host_socket)
        .env("SSH_AUTH_SOCK", &host_socket)
        .args([
            "spawn",
            "--stdio",
            "capture",
            "--filesystem",
            "sandbox",
            "--stage",
        ])
        .arg(temporary.path().join("stage"))
        .current_dir(&workspace)
        .arg("--")
        .arg(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "unix_socket_probe_agent"])
        .output()
        .unwrap();

    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let bundle = RunBundle::read(&only_run(&stage_root(&run_home))).unwrap();
    if !bundle.safety.filesystem_non_bypassable
        && String::from_utf8_lossy(&output.stderr).contains("falling back to host process")
    {
        eprintln!("skipping: synthetic root unavailable on this test host");
        return;
    }
    assert!(bundle.safety.filesystem_non_bypassable);
}

#[test]
fn safe_run_reaps_setsid_double_fork_descendants_after_success() {
    if skip_if_rootless_runtime_is_explicitly_optional() {
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    fs::create_dir_all(&workspace).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_RUN_HOME", &run_home)
        .args([
            "spawn",
            "--stdio",
            "capture",
            "--filesystem",
            "sandbox",
            "--stage",
        ])
        .arg(temporary.path().join("stage"))
        .current_dir(&workspace)
        .arg("--")
        .arg(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "daemon_escape_probe_agent"])
        .output()
        .unwrap();

    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let bundle = RunBundle::read(&only_run(&stage_root(&run_home))).unwrap();
    let socket = bundle
        .filesystem
        .as_ref()
        .expect("staged filesystem")
        .upper
        .join("daemon.sock");
    assert!(socket.exists(), "daemon did not publish its socket");
    let error = UnixStream::connect(&socket)
        .expect_err("setsid/double-fork descendant survived successful Run completion");
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
    ));
}

/// Re-enter this integration-test executable as an Agent that deliberately
/// escapes its process group and double-forks a long-lived Unix listener.
#[test]
#[ignore]
fn daemon_escape_probe_agent() {
    let status = Command::new("/usr/bin/setsid")
        .arg("--fork")
        .arg(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "daemon_listener_agent"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("launch setsid daemon");
    assert!(status.success(), "setsid launcher failed: {status}");
    for _ in 0..500 {
        if Path::new("daemon.ready").is_file() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("daemon did not become ready");
}

#[test]
#[ignore]
fn daemon_listener_agent() {
    let listener = UnixListener::bind("daemon.sock").expect("bind daemon socket");
    fs::write("daemon.ready", b"ready").expect("publish daemon readiness");
    loop {
        match listener.accept() {
            Ok((_stream, _address)) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => panic!("daemon listener failed: {error}"),
        }
    }
}

#[test]
fn sandboxed_agent_may_legitimately_exit_with_reserved_launcher_code() {
    if skip_if_rootless_runtime_is_explicitly_optional() {
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let run_home = temporary.path().join("runs");
    fs::create_dir_all(&workspace).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_RUN_HOME", &run_home)
        .args(["spawn", "--stdio", "capture", "--stage"])
        .arg(temporary.path().join("stage"))
        .current_dir(&workspace)
        .args(["--", "/bin/sh", "-c", "exit 125"])
        .output()
        .unwrap();
    if skip_if_user_namespaces_are_explicitly_optional(&run_home, &output) {
        return;
    }

    assert!(!output.status.success());
    let bundle = RunBundle::read(&only_run(&stage_root(&run_home))).unwrap();
    assert_eq!(bundle.run.exit_code, Some(SANDBOX_SETUP_EXIT_CODE));
    assert_eq!(
        bundle.run.failure.as_ref().map(|failure| failure.kind),
        Some(RunFailureKind::ProcessExit)
    );
    assert!(
        bundle
            .run
            .warnings
            .iter()
            .all(|warning| warning != "pvisor.sandbox.setup_failed")
    );
}

/// Re-enter this integration-test executable as the untrusted Agent so the
/// socket regression does not depend on Python, curl, socat, or netcat.
#[test]
#[ignore]
fn unix_socket_probe_agent() {
    let Some(path) = std::env::var_os("PVISOR_SOCKET_PROBE") else {
        return;
    };
    let error = UnixStream::connect(path).expect_err("ungranted host socket must be hidden");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);

    let inherited_ssh_agent =
        std::env::var_os("SSH_AUTH_SOCK").expect("the Agent should still see inherited metadata");
    let error = UnixStream::connect(inherited_ssh_agent)
        .expect_err("ambient SSH signing authority must not be projected");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);

    let agentctl = std::env::var_os(pvisor::AGENTCTL_ENDPOINT_ENV)
        .expect("pVisor must inject its Run-scoped AgentCtl endpoint");
    UnixStream::connect(agentctl).expect("the explicitly projected AgentCtl socket must work");
}

#[test]
fn internal_launcher_reports_setup_failure_with_reserved_status() {
    let temporary = tempfile::tempdir().unwrap();
    let plan = temporary.path().join("invalid-plan.json");
    fs::write(&plan, "not-json").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .arg("__pvisor-sandbox-exec")
        .arg("--")
        .arg("/bin/true")
        .env("PVISOR_INTERNAL_SANDBOX_PLAN", &plan)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(SANDBOX_SETUP_EXIT_CODE));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("decode rootless sandbox plan"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// All fixture mounts are created in the child's private user/mount namespace.
// This exercises the actual launcher without requiring workspace FUSE staging.
#[test]
fn inherited_nested_state_mount_fails_closed_before_agent_execution() {
    let output = Command::new("unshare")
        .args([
            "--user",
            "--map-root-user",
            "--mount",
            "--fork",
            "--propagation",
            "private",
        ])
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "inherited_nested_state_mount_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("PVISOR_NESTED_MOUNT_FIXTURE", "1")
        .output()
        .expect("run isolated mount fixture using util-linux unshare");
    let stderr = String::from_utf8_lossy(&output.stderr);
    if std::env::var_os("PVISOR_TEST_ALLOW_NO_USERNS").is_some()
        && (stderr.trim() == "unshare: unshare failed: Operation not permitted"
            || stderr.trim() == "unshare: unshare failed: Permission denied")
    {
        eprintln!(
            "skipping: isolated nested-mount fixture cannot create user/mount namespaces: {stderr}"
        );
        return;
    }
    assert!(
        output.status.success(),
        "isolated fixture failed:\n{stderr}\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
#[ignore = "helper re-entered only in a private user/mount namespace"]
fn inherited_nested_state_mount_fixture() {
    if std::env::var_os("PVISOR_NESTED_MOUNT_FIXTURE").is_none() {
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("state root");
    let nested = home.join("nested mount");
    let root = temporary.path().join("sandbox");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir(&root).unwrap();
    let nested_c = std::ffi::CString::new(nested.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(
        unsafe {
            libc::mount(
                c"nested-state-fixture".as_ptr(),
                nested_c.as_ptr(),
                c"tmpfs".as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV,
                c"size=1m".as_ptr().cast(),
            )
        },
        0,
        "fixture tmpfs mount: {}",
        std::io::Error::last_os_error()
    );
    fs::write(nested.join("unchanged"), b"nested lower").unwrap();
    fs::write(home.join("unchanged"), b"state lower").unwrap();
    let attestation = temporary.path().join("attestation");
    fs::write(&attestation, b"").unwrap();
    let mut attestation_reader = fs::File::open(&attestation).unwrap();
    let plan = temporary.path().join("plan.json");
    fs::write(
        &plan,
        serde_json::to_vec(&serde_json::json!({
            "root": root,
            "cwd": home,
            "attestation": attestation,
            "read_only": [],
            "read_write": [],
            "staged_roots": [home],
            "network": "Ambient",
            "filesystem_isolated": true
        }))
        .unwrap(),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PVISOR_INTERNAL_SANDBOX_PLAN", &plan)
        .args([
            "__pvisor-sandbox-exec",
            "--",
            "/bin/sh",
            "-c",
            "printf executed > agent-executed",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(SANDBOX_SETUP_EXIT_CODE),
        "{stderr}"
    );
    assert!(stderr.contains("mount staged state OverlayFS"), "{stderr}");
    assert!(stderr.contains("state root mountpoint="), "{stderr}");
    assert!(
        stderr.contains("fstype=") && stderr.contains("source="),
        "{stderr}"
    );
    assert!(
        stderr.contains("unsupported inherited nested mounts (1 strict descendants"),
        "{stderr}"
    );
    assert!(
        stderr.contains("nested mount") && stderr.contains("nested-state-fixture"),
        "{stderr}"
    );
    assert!(
        stderr.contains("os error"),
        "expected original OverlayFS errno: {stderr}"
    );
    let mut attested = Vec::new();
    std::io::Read::read_to_end(&mut attestation_reader, &mut attested).unwrap();
    assert!(attested.is_empty(), "setup must not attest execution");
    assert!(
        !attestation.exists(),
        "attestation must be unlinked before setup"
    );
    assert!(!home.join("agent-executed").exists());
    assert_eq!(fs::read(home.join("unchanged")).unwrap(), b"state lower");
    assert_eq!(fs::read(nested.join("unchanged")).unwrap(), b"nested lower");
    // No host unmount: namespace teardown owns fixture mount cleanup.
}

#[test]
fn namespace_setup_failure_classifier_is_narrow() {
    assert!(user_namespaces_are_unavailable(
        "pVisor local sandbox setup failed: initialize rootless user and mount namespaces: unshare user namespace: Operation not permitted (os error 1)"
    ));
    assert!(user_namespaces_are_unavailable(
        "pVisor local sandbox setup failed: initialize rootless user and mount namespaces: unshare user namespace: Permission denied (os error 13)"
    ));
    assert!(!user_namespaces_are_unavailable(
        "apply Landlock rules: Permission denied (os error 13)"
    ));
    assert!(!user_namespaces_are_unavailable(
        "initialize rootless user and mount namespaces: No such file or directory (os error 2)"
    ));
    assert!(!user_namespaces_are_unavailable(
        "initialize rootless user and mount namespaces: unshare mount namespace: Operation not permitted (os error 1)"
    ));
}
