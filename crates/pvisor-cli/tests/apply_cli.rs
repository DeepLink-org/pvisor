//! External apply contracts, including real keyboard input and terminal restoration.
#![cfg(unix)]
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Fixture {
    root: tempfile::TempDir,
    stage: PathBuf,
    target: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let stage = root.path().join("stage");
        let target = root.path().join("project");
        fs::create_dir(&target).unwrap();
        for dir in ["upper", "work", "merged"] {
            fs::create_dir_all(stage.join(dir)).unwrap();
        }
        let record: pvisor::RunRecord = serde_json::from_value(json!({
            "schema_version":1,"run_id":"apply-cli-job","session_id":"apply-cli-session","attempt_id":"apply-cli-attempt",
            "agent":"test","pid":0,"command":["/bin/true"],"state":"completed",
            "started_at_unix_ms":10,"finished_at_unix_ms":20,"storage":stage,"workspace":target,"network":{},
            "overlay":{"id":"apply-cli-job","generation":7,"target":target,
                "upper":{"upper_dir":stage.join("upper"),"work_dir":stage.join("work")},
                "merged_dir":stage.join("merged"),"stage_dir":stage,"auto_apply":false,"state":"staged"}
        })).unwrap();
        record.write().unwrap();
        // Launch initializes observation locks; legacy fixtures must do the same
        // before preview, which must never create or initialize lock files.
        for name in ["job.lock", "lease.lock"] {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(stage.join(name))
                .unwrap();
            assert_eq!(
                fs::metadata(stage.join(name)).unwrap().mode() & 0o7777,
                0o600
            );
        }
        let journal = stage.join("preimages");
        pvisor_overlay_core::stage::begin(&journal, pvisor_core::overlay::StageDurability::Strict)
            .unwrap();
        let core = pvisor_overlay_core::OverlayCore::new_with_exclusions_and_preimages(
            vec![target.clone()],
            stage.join("upper"),
            Some(stage.join("work")),
            vec![],
            Some(journal.clone()),
        )
        .unwrap();
        for name in ["keep.txt", "skip.txt"] {
            core.observe_read(Path::new(name)).unwrap();
        }
        drop(core);
        fs::write(stage.join("upper/keep.txt"), b"kept\n").unwrap();
        fs::write(stage.join("upper/skip.txt"), b"skipped\n").unwrap();
        pvisor_overlay_core::stage::seal(&stage.join("upper"), &journal).unwrap();
        Self {
            root,
            stage,
            target,
        }
    }
    fn run(&self, options: &[&str]) -> Output {
        cli(self.root.path(), &self.stage, options)
    }
    fn unchanged(&self) {
        assert_eq!(fs::read_dir(&self.target).unwrap().count(), 0);
        assert_eq!(
            fs::read(self.stage.join("upper/keep.txt")).unwrap(),
            b"kept\n"
        );
        assert!(
            pvisor_overlay_core::apply::load_apply_records(&self.stage)
                .unwrap()
                .is_empty()
        );
    }
}
fn cli(root: &Path, source: &Path, options: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .arg("apply")
        .arg(source)
        .args(options)
        .current_dir(root)
        .env("HOME", root)
        .env_remove("PVISOR_STARTUP_TIMING")
        .env_remove("PVISOR_STARTUP_PROFILE")
        .output()
        .unwrap()
}
fn object(output: &Output, code: i32) -> Value {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {output:?}"))
}

#[derive(Debug, PartialEq, Eq)]
struct FileSnapshot {
    mode: u32,
    device: u64,
    inode: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    contents: Vec<u8>,
}
fn tree_snapshot(root: &Path) -> BTreeMap<PathBuf, FileSnapshot> {
    fn visit(root: &Path, path: &Path, snapshot: &mut BTreeMap<PathBuf, FileSnapshot>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        snapshot.insert(
            path.strip_prefix(root).unwrap().to_path_buf(),
            FileSnapshot {
                mode: metadata.mode(),
                device: metadata.dev(),
                inode: metadata.ino(),
                modified: (metadata.mtime(), metadata.mtime_nsec()),
                changed: (metadata.ctime(), metadata.ctime_nsec()),
                contents: if metadata.is_file() {
                    fs::read(path).unwrap()
                } else {
                    vec![]
                },
            },
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), snapshot);
            }
        }
    }
    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}

#[test]
fn missing_locks_preview_is_blocked_without_creating_or_modifying_files() {
    for missing in [
        vec!["job.lock"],
        vec!["lease.lock"],
        vec!["job.lock", "lease.lock"],
    ] {
        let f = Fixture::new();
        for name in &missing {
            fs::remove_file(f.stage.join(name)).unwrap();
        }
        let before = tree_snapshot(f.root.path());
        let output = f.run(&["--all", "--dry-run", "--json"]);
        let value = object(&output, 3);
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["mode"], "preview");
        assert_eq!(value["status"], "blocked");
        assert!(value["execution"].is_null());
        assert!(
            value["blockers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|blocker| blocker
                    .as_str()
                    .is_some_and(|text| text.contains("preview never initializes locks"))),
            "{value}"
        );
        assert_eq!(tree_snapshot(f.root.path()), before, "missing {missing:?}");
        for name in &missing {
            assert!(!f.stage.join(name).exists());
        }
        f.unchanged();
    }
}

#[test]
fn parser_non_tty_and_json_errors_are_inert() {
    let f = Fixture::new();
    for options in [
        vec![],
        vec!["--exclude", "*.txt"],
        vec!["-i"],
        vec!["--all", "--path", "keep.txt"],
        vec!["--all", "--include", "*.txt"],
        vec!["--all", "--diff"],
        vec!["--all", "-i", "--dry-run"],
        vec!["--all", "--force"],
        vec!["--all", "--yes"],
        vec!["--all", "--export"],
        vec!["--path", "../escape"],
        vec!["--include", "/absolute/*"],
        vec!["--all", "--target", "/tmp/removed-target"],
        vec!["--all", "-o", "removed-store"],
        vec!["--all", "--output-dir", "removed-store"],
    ] {
        let out = f.run(&options);
        assert_eq!(out.status.code(), Some(2), "{options:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{out:?}");
    }
    for options in [
        vec!["--json"],
        vec!["--json", "--exclude", "*.txt"],
        vec!["--json", "--all", "-i"],
        vec!["--json", "--all", "--path", "keep.txt"],
    ] {
        let value = object(&f.run(&options), 2);
        assert_eq!(value["status"], "error");
        assert!(value["source"].is_null());
    }
    let missing = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["apply", "--json", "--all"])
        .output()
        .unwrap();
    object(&missing, 2);
    f.unchanged();
}

#[test]
fn json_preview_all_exclude_store_and_direct_execution() {
    let f = Fixture::new();
    let before = tree_snapshot(f.root.path());
    for _ in 0..2 {
        let output = f.run(&[
            "--all",
            "--exclude",
            "skip.*",
            "--dry-run",
            "--json",
            "--store",
            f.root.path().to_str().unwrap(),
        ]);
        let value = object(&output, 0);
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["mode"], "preview");
        assert_eq!(value["status"], "ready");
        assert_eq!(value["selected"].as_array().unwrap().len(), 1);
        assert_eq!(value["selected"][0]["path"], "keep.txt");
        assert!(value["execution"].is_null());
        assert_eq!(tree_snapshot(f.root.path()), before);
        f.unchanged();
    }
    // Non-TTY explicit positives execute without any prompt and without outer-lock routing.
    let output = f.run(&["--all", "--exclude", "skip.*", "--json"]);
    let value = object(&output, 0);
    assert_eq!(value["status"], "applied");
    assert_eq!(fs::read(f.target.join("keep.txt")).unwrap(), b"kept\n");
    assert!(!f.target.join("skip.txt").exists());
    assert!(f.stage.join("upper/skip.txt").exists());
}

#[test]
fn upper_and_internal_stage_sources_are_rejected_without_writes() {
    let f = Fixture::new();
    let before = tree_snapshot(f.root.path());
    for source in ["upper", "upper/keep.txt", "run.json", "merged", "work"] {
        for dry_run in [true, false] {
            let mut options = vec!["--all", "--json"];
            if dry_run {
                options.push("--dry-run");
            }
            let output = cli(f.root.path(), &f.stage.join(source), &options);
            let value = object(&output, 3);
            assert_eq!(value["status"], "blocked", "{source}: {value}");
            assert!(value["execution"].is_null());
            assert!(
                value["blockers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|blocker| blocker
                        .as_str()
                        .is_some_and(|text| text.contains("apply source"))),
                "{source}: {value}"
            );
            assert_eq!(tree_snapshot(f.root.path()), before, "{source}");
            f.unchanged();
        }
    }
}

#[test]
fn removed_target_and_storage_aliases_are_rejected_as_json_argument_errors() {
    let f = Fixture::new();
    let before = tree_snapshot(f.root.path());
    for flag in ["--target", "-o", "--output-dir"] {
        let output = f.run(&["--all", flag, f.target.to_str().unwrap(), "--json"]);
        let value = object(&output, 2);
        assert_eq!(value["status"], "error");
        assert!(value["source"].is_null());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("unexpected argument"),
            "{output:?}"
        );
        assert_eq!(tree_snapshot(f.root.path()), before);
    }
}

#[test]
fn stage_source_paths_includes_and_no_changes() {
    let f = Fixture::new();
    let preview = object(&f.run(&["--path", "keep.txt", "--dry-run", "--json"]), 0);
    assert_eq!(preview["source"]["stage"], f.stage.to_str().unwrap());
    assert_eq!(preview["selected"][0]["path"], "keep.txt");
    f.unchanged();
    let output = f.run(&["--include", "*.txt", "--json"]);
    assert_eq!(object(&output, 0)["status"], "applied");
    assert_eq!(fs::read(f.target.join("keep.txt")).unwrap(), b"kept\n");
    assert_eq!(fs::read(f.target.join("skip.txt")).unwrap(), b"skipped\n");
    assert_eq!(
        object(&f.run(&["--all", "--json"]), 0)["status"],
        "no_changes"
    );
}

#[test]
fn help_is_grouped_and_text_distinguishes_preflight_no_matches_and_success() {
    let f = Fixture::new();
    let output = f.run(&["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    let headings = ["Scope:", "Selection:", "Preview:", "Output:", "Storage:"];
    let positions = headings.map(|heading| {
        help.find(heading)
            .unwrap_or_else(|| panic!("{heading}: {help}"))
    });
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{help}");
    let output = f.run(&["--path", "missing"]);
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("No matching staged changes"), "{text}");
    assert!(text.contains("Source: apply-cli-job"), "{text}");
    assert!(
        text.contains(&format!("Stage: {}", f.stage.display())),
        "{text}"
    );
    let output = f.run(&["--all"]);
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Apply ID: "), "{text}");
    assert!(text.contains("Tests were not run automatically"), "{text}");
    assert!(text.contains("not a transaction"), "{text}");
    assert!(!text.contains("No target changes"), "{text}");

    let f = Fixture::new();
    fs::write(f.target.join("keep.txt"), b"concurrent\n").unwrap();
    let output = f.run(&["--path", "keep.txt"]);
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("No target changes were made (blocked during preflight)"),
        "{text}"
    );
    assert!(!text.contains("No matching staged changes"), "{text}");
}

#[test]
fn unresolved_source_is_one_json_error_and_diff_is_bounded_text() {
    let f = Fixture::new();
    let missing = cli(
        f.root.path(),
        &f.root.path().join("missing"),
        &["--all", "--json"],
    );
    let value = object(&missing, 1);
    assert!(value["source"].is_null());
    assert!(!missing.stderr.is_empty());
    let output = f.run(&["--path", "keep.txt", "--dry-run", "--diff"]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("+kept"));
    fs::write(f.stage.join("upper/keep.txt"), b"binary\0payload").unwrap();
    let output = f.run(&["--path", "keep.txt", "--dry-run", "--diff"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("binary (text diff unavailable)"));
    assert_eq!(fs::read_dir(&f.target).unwrap().count(), 0);
}

#[test]
fn preview_conflict_never_publishes_an_apply_ledger() {
    let f = Fixture::new();
    // The staged creation's preimage is absent. Concurrent target creation conflicts.

    fs::write(f.target.join("keep.txt"), b"concurrent\n").unwrap();
    let output = f.run(&["--path", "keep.txt", "--dry-run", "--json"]);
    let value = object(&output, 3);
    assert_eq!(value["status"], "blocked");
    assert!(!value["blockers"].as_array().unwrap().is_empty());
    assert_eq!(
        fs::read(f.target.join("keep.txt")).unwrap(),
        b"concurrent\n"
    );
    assert!(
        pvisor_overlay_core::apply::load_apply_records(&f.stage)
            .unwrap()
            .is_empty()
    );
}

const PTY: &str = r#"
import os, pty, select, signal, subprocess, sys, termios, time
binary, root, stage, mode = sys.argv[1:]
pid, master = pty.fork()
if pid == 0:
    os.chdir(root)
    initial = termios.tcgetattr(0)
    initial[3] |= termios.ICANON | termios.ECHO | termios.ISIG
    initial[0] |= termios.IXON | termios.ICRNL
    # Non-default values catch restoration to defaults instead of the saved mode.
    initial[6][termios.VMIN] = 7
    initial[6][termios.VTIME] = 3
    termios.tcsetattr(0, termios.TCSANOW, initial)
    # Darwin sets PENDIN on a noncanonical -> canonical transition. A readiness
    # check processes pending input without reading/flushing it or masking flags.
    def terminal_state():
        select.select([0], [], [], 0)
        return termios.tcgetattr(0)
    saved = terminal_state()
    args = [binary, 'apply', stage]
    if mode == 'exclude': args += ['-i', '--all', '--exclude', 'keep.*']
    if mode == 'exclude_conflict': args += ['-i', '--exclude', 'skip.*']
    child = subprocess.Popen(args)
    code = child.wait()
    actual = terminal_state()
    restored = actual == saved
    if not restored:
        fields = ['iflag', 'oflag', 'cflag', 'lflag', 'ispeed', 'ospeed', 'cc']
        differences = [(name, before, after)
                       for name, before, after in zip(fields, saved, actual)
                       if before != after]
        os.write(1, ('TERMIOS DIFF (field, saved, actual): %r\n' % differences).encode())
    os.write(1, ('RESTORED:%s EXIT:%s\n' % (int(restored), code)).encode())
    os._exit(0 if restored and code == (0 if mode == 'apply' else 130) else 41)
output = bytearray()
state = 0
deadline = time.monotonic() + 20
try:
    while time.monotonic() < deadline:
        ready, _, _ = select.select([master], [], [], .1)
        if ready:
            try: chunk = os.read(master, 65536)
            except OSError: break
            if not chunk: break
            output.extend(chunk)
            assert len(output) < 200000, bytes(output)
            if state == 0 and b'Enter preview, q quit' in output:
                if mode == 'quit': os.write(master, b'q'); state = 5
                elif mode == 'exclude':
                    output.clear(); os.write(master, b'n \r'); state = 4
                elif mode == 'escape':
                    output.clear(); os.write(master, b'\x1b'); state = 7
                else:
                    assert b'[x]' not in output, bytes(output)
                    os.write(master, b'an\x1b[B\x1b[A \r'); state = 1
            if state == 1 and b'[a] apply' in output:
                if mode == 'exclude_conflict':
                    assert b'blocker:' in output and b'(apply blocked)' in output, bytes(output)
                    assert b'No target changes were made (blocked during preflight)' in output, bytes(output)
                    os.write(master, b'aq'); state = 5
                else: assert b'selected: 1' in output, bytes(output)
                if mode == 'exclude_conflict': pass
                elif mode == 'refresh':
                    with open(os.path.join(stage, 'upper', 'fresh.txt'), 'wb') as f: f.write(b'fresh\n')
                    output.clear(); os.write(master, b'b'); state = 6
                elif mode == 'apply': os.write(master, b'a'); state = 5
                elif mode == 'changed':
                    with open(os.path.join(stage, 'upper', 'keep.txt'), 'wb') as f: f.write(b'changed after preview\n')
                    output.clear(); os.write(master, b'a'); state = 3
                else: os.write(master, b'b'); state = 2; output.clear()
            if state == 2 and b'Enter preview, q quit' in output:
                os.write(master, b'q'); state = 5
            if state == 4 and b'Select at least one path.' in output:
                output.clear(); os.write(master, b'a\r'); state = 8
            if state == 8 and b'[a] apply' in output:
                assert b'selected: 1' in output and b'"skip.txt"' in output, bytes(output)
                assert b'"keep.txt"' not in output.split(b'selected: 1', 1)[1], bytes(output)
                os.write(master, b'q'); state = 5
            if state == 6 and b'Enter preview, q quit' in output and b'fresh.txt' in output:
                os.write(master, b'q'); state = 5
            if state == 7 and b'Enter preview, q quit' in output:
                os.write(master, b'q'); state = 5
            if state == 3 and b'plan changed' in output and b'[a] apply' in output:
                assert not os.path.exists(os.path.join(root, 'project', 'keep.txt')), bytes(output)
                os.write(master, b'q'); state = 5
        if b'RESTORED:' in output: break
    else: raise AssertionError('PTY timeout: ' + repr(bytes(output)))
    _, status = os.waitpid(pid, 0)
    assert status == 0, (status, bytes(output))
    assert b'RESTORED:1' in output, bytes(output)

except BaseException:
    try: os.kill(pid, signal.SIGKILL)
    except ProcessLookupError: pass
    raise
finally: os.close(master)
print(output.decode(errors='replace'))
"#;
#[test]
fn real_pty_quit_back_apply_and_exclusion_restore_terminal() {
    for mode in [
        "quit",
        "back",
        "apply",
        "exclude",
        "changed",
        "refresh",
        "escape",
        "exclude_conflict",
    ] {
        let f = Fixture::new();
        if mode == "exclude_conflict" {
            fs::write(f.target.join("keep.txt"), b"concurrent\n").unwrap();
        }
        let output = Command::new("python3")
            .args(["-c", PTY, env!("CARGO_BIN_EXE_pvisor")])
            .arg(f.root.path())
            .arg(&f.stage)
            .arg(mode)
            .output()
            .unwrap();
        assert!(output.status.success(), "{mode}: {output:?}");
        if mode == "apply" {
            assert_eq!(fs::read(f.target.join("keep.txt")).unwrap(), b"kept\n");
            assert!(!f.target.join("skip.txt").exists());
        } else if mode == "exclude_conflict" {
            assert_eq!(
                fs::read(f.target.join("keep.txt")).unwrap(),
                b"concurrent\n"
            );
            assert!(!f.target.join("skip.txt").exists());
            assert!(
                pvisor_overlay_core::apply::load_apply_records(&f.stage)
                    .unwrap()
                    .is_empty()
            );
        } else if mode == "changed" {
            assert_eq!(fs::read_dir(&f.target).unwrap().count(), 0);
            assert!(
                pvisor_overlay_core::apply::load_apply_records(&f.stage)
                    .unwrap()
                    .is_empty()
            );
        } else {
            f.unchanged();
        }
    }
}
