//! Explicit staged-bind routing through the CLI; persisted fixtures need no VM.
#![cfg(target_os = "linux")]
use pvisor::{RunBundle, RunRecord};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Fixture {
    root: tempfile::TempDir,
    store: PathBuf,
    instance: Value,
    stages: Vec<PathBuf>,
    sources: Vec<PathBuf>,
}
impl Fixture {
    fn new(count: usize) -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = root.path().join("docker");
        pvisor::docker::Store::open(store.clone()).unwrap();
        let id = "a".repeat(64);
        let directory = store.join("instances").join(&id);
        fs::create_dir_all(&directory).unwrap();
        let mut mounts = Vec::new();
        let mut stages = Vec::new();
        let mut sources = Vec::new();
        for index in 0..count {
            let source = root.path().join(format!("project-{index}"));
            fs::create_dir(&source).unwrap();
            let stage = directory.join("stages/generation").join(index.to_string());
            for dir in ["upper", "work", "merged"] {
                fs::create_dir_all(stage.join(dir)).unwrap();
            }
            let target = format!("/workspace-{index}");
            let binding = json!({"version":1,"store":store,"instance_id":id,"generation":"generation",
                "mount_index":index,"guest_target":target,"source":source});
            let record: RunRecord = serde_json::from_value(json!({
                "schema_version":2,"completion_protocol":1,"run_id":format!("bind-{index}"),
                "session_id":id,"attempt_id":format!("attempt-{index}"),"agent":"fixture","pid":0,
                "command":["/bin/true"],"state":"completed","started_at_unix_ms":10,"finished_at_unix_ms":20,
                "storage":stage,"workspace":source,"network":{},"overlay_lowers":[source],
                "project_apply_scope":pvisor::job_service::ProjectApplyScope::capture(&source).unwrap(),
                "orchestration":{"pvisor.docker_staged_bind":binding},
                "overlay":{"id":format!("bind-{index}"),"generation":0,"target":source,
                    "upper":{"upper_dir":stage.join("upper"),"work_dir":stage.join("work")},
                    "merged_dir":stage.join("merged"),"stage_dir":stage,"auto_apply":false,"state":"staged"}
            })).unwrap();
            record.write().unwrap();
            for name in ["job.lock", "lease.lock"] {
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(stage.join(name))
                    .unwrap();
            }
            let journal = stage.join("preimages");
            pvisor_overlay_core::stage::begin(
                &journal,
                pvisor_core::overlay::StageDurability::Strict,
            )
            .unwrap();
            let core = pvisor_overlay_core::OverlayCore::new_with_exclusions_and_preimages(
                vec![source.clone()],
                stage.join("upper"),
                Some(stage.join("work")),
                vec![],
                Some(journal.clone()),
            )
            .unwrap();
            core.observe_read(Path::new("result")).unwrap();
            drop(core);
            fs::write(stage.join("upper/result"), format!("result-{index}")).unwrap();
            pvisor_overlay_core::stage::seal(&stage.join("upper"), &journal).unwrap();
            let result = json!({"run_id":record.run_id,"attempt_id":record.attempt_id,"state":"completed",
                "started_at_unix_ms":10,"finished_at_unix_ms":20,"exit_code":0});
            let snapshot = serde_json::from_value(
                json!({"run_id":record.run_id,"attempt_id":record.attempt_id,
                "directive":{"kind":"continue"},"clients":[]}),
            )
            .unwrap();
            RunBundle::capture(
                &record,
                &serde_json::from_value(result.clone()).unwrap(),
                snapshot,
                false,
            )
            .unwrap()
            .write(&stage)
            .unwrap();
            fs::write(stage.join("completion.json"), serde_json::to_vec(&json!({"version":1,"result":result,
                "bundle_blake3":blake3::hash(&fs::read(RunBundle::path(&stage)).unwrap()).to_hex().to_string()})).unwrap()).unwrap();
            mounts.push(json!({"source":source,"target":target,"read_only":false,"staged":true,"volume":null}));
            sources.push(source);
            stages.push(stage);
        }
        let instance = json!({"schema":1,"id":id,"name":"staged","generation":"generation","state":"exited",
            "exit_code":0,"created_ms":10,"argv":["/bin/true"],"env":{},"cwd":"/","user":"",
            "image":{"reference":"fixture","id":"fixture","digest":"fixture","platform":"linux/amd64",
                "cache":{"location":store.join("images"),"handle":"fixture"},"env":{},"entrypoint":[],
                "cmd":["/bin/true"],"cwd":"/","user":"","stop_signal":15,"volumes":[],"created_ms":10},
            "config":{"name":"staged","image":"fixture","command":[],"entrypoint":null,"env":{},"cwd":null,
                "user":null,"mounts":mounts,"ports":[],"cpus":1,"memory_mib":128,"auto_remove":false,
                "interactive":false,"tty":null,"network":false}});
        fs::write(
            store.join("results").join(format!("{id}.json")),
            serde_json::to_vec(&json!({"generation":"generation","code":0})).unwrap(),
        )
        .unwrap();
        let fixture = Self {
            root,
            store,
            instance,
            stages,
            sources,
        };
        fixture.save();
        fixture
    }
    fn save(&self) {
        fs::write(
            self.store
                .join("instances")
                .join(self.instance["id"].as_str().unwrap())
                .join("instance.json"),
            serde_json::to_vec(&self.instance).unwrap(),
        )
        .unwrap();
    }
    fn call(&self, operation: &str, selector: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .arg(operation)
            .arg(selector)
            .args(args)
            .current_dir(self.root.path())
            .env("HOME", self.root.path())
            .env("PVISOR_RUN_HOME", self.root.path().join("runs"))
            .env("PVISOR_DOCKER_HOME", &self.store)
            .env_remove("PVISOR_STARTUP_TIMING")
            .env_remove("PVISOR_STARTUP_PROFILE")
            .output()
            .unwrap()
    }
    fn named(&self, operation: &str, args: &[&str]) -> Output {
        self.call(operation, Path::new("staged"), args)
    }
}
fn object(output: &Output, code: i32) -> Value {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {output:?}"))
}
fn success(output: &Output) {
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn single_bind_auto_selects_for_review_apply_and_drop() {
    let f = Fixture::new(1);
    let review = object(&f.named("review", &["--json"]), 0);
    assert_eq!(review["run"]["run_id"], "bind-0");
    let preview = object(&f.named("apply", &["--all", "--dry-run", "--json"]), 0);
    assert_eq!(preview["source"]["stage"], json!(f.stages[0]));
    assert!(!f.sources[0].join("result").exists());
    object(&f.named("apply", &["--all", "--json"]), 0);
    assert_eq!(
        fs::read_to_string(f.sources[0].join("result")).unwrap(),
        "result-0"
    );
    assert!(
        !f.named("drop", &[]).status.success(),
        "drop must not undo an applied stage"
    );
    assert_eq!(
        fs::read_to_string(f.sources[0].join("result")).unwrap(),
        "result-0"
    );
    let pending = Fixture::new(1);
    success(&pending.named("drop", &[]));
    assert!(!pending.stages[0].join("upper/result").exists());
    assert!(!pending.sources[0].join("result").exists());
}

#[test]
fn multiple_binds_require_exact_guest_selector_and_mutate_only_selected_stage() {
    let f = Fixture::new(2);
    for operation in ["review", "apply", "drop"] {
        let options = if operation == "apply" {
            vec!["--all", "--json"]
        } else {
            vec![]
        };
        let output = f.named(operation, &options);
        assert!(!output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("exactly one staged bind"),
            "{output:?}"
        );
        if operation == "apply" {
            object(&output, 1);
        }
        for selector in [
            "/workspace",
            "/workspace-1/child",
            "/missing",
            "workspace-1",
        ] {
            let mut options = options.clone();
            options.extend(["--mount", selector]);
            let output = f.named(operation, &options);
            assert!(!output.status.success(), "{output:?}");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("exactly name"),
                "{output:?}"
            );
            if operation == "apply" {
                object(&output, 1);
            }
        }
    }
    object(
        &f.named("review", &["--mount", "/workspace-1", "--json"]),
        0,
    );
    object(
        &f.named("apply", &["--mount", "/workspace-1", "--all", "--json"]),
        0,
    );
    assert!(!f.sources[0].join("result").exists());
    assert_eq!(
        fs::read_to_string(f.sources[1].join("result")).unwrap(),
        "result-1"
    );
    success(&f.named("drop", &["--mount", "/workspace-0"]));
    assert!(!f.stages[0].join("upper/result").exists());
    assert_eq!(
        fs::read_to_string(f.sources[1].join("result")).unwrap(),
        "result-1"
    );
}

#[test]
fn container_rootfs_is_rejected_by_name_and_direct_stage_path() {
    let f = Fixture::new(1);
    for source in [Path::new("staged"), f.stages[0].as_path()] {
        let output = f.call("apply", source, &["--rootfs", "--all", "--json"]);
        let response = object(&output, 3);
        assert!(
            response["blockers"].to_string().contains("--rootfs"),
            "{response}"
        );
        assert!(!f.sources[0].join("result").exists());
    }
}

#[test]
fn active_or_uncommitted_container_selection_fails_closed() {
    use fs2::FileExt;
    let mut f = Fixture::new(1);
    let owner = fs::File::create(
        f.store
            .join("instances")
            .join(f.instance["id"].as_str().unwrap())
            .join("owner.lock"),
    )
    .unwrap();
    owner.lock_exclusive().unwrap();
    for state in ["running", "created"] {
        f.instance["state"] = json!(state);
        f.save();
        for operation in ["review", "apply", "drop"] {
            let args = if operation == "apply" {
                vec!["--all", "--json"]
            } else {
                vec![]
            };
            let output = f.named(operation, &args);
            assert!(!output.status.success(), "{output:?}");
            if operation == "apply" {
                object(&output, 1);
            }
        }
    }
    drop(owner);
    f.instance["state"] = json!("exited");
    f.save();
    fs::remove_file(f.stages[0].join("completion.json")).unwrap();
    object(&f.named("apply", &["--all", "--json"]), 1);
    assert!(f.stages[0].join("upper/result").exists());
    assert!(!f.sources[0].join("result").exists());
}

#[test]
fn job_sources_are_preserved_and_mount_explicitly_selects_container() {
    let f = Fixture::new(1);
    // A corrupt same-named Job path must not be reinterpreted as a container.
    let job = f.root.path().join("staged");
    fs::create_dir(&job).unwrap();
    fs::write(job.join("run.json"), b"corrupt").unwrap();
    for operation in ["review", "apply", "drop"] {
        let args = if operation == "apply" {
            vec!["--all", "--json"]
        } else {
            vec![]
        };
        let output = f.named(operation, &args);
        assert!(!output.status.success(), "{output:?}");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("No such container"));
        if operation == "apply" {
            object(&output, 1);
        }
        let mut explicit = args.clone();
        if operation == "apply" {
            explicit.push("--dry-run");
        }
        explicit.extend(["--mount", "/workspace-0"]);
        success(&f.named(operation, &explicit));
    }
}

#[test]
fn default_job_corruption_and_ambiguity_never_fall_back() {
    for corrupt in [true, false] {
        let f = Fixture::new(1);
        for name in ["one", "two"] {
            let storage = f.root.path().join("runs").join(name);
            fs::create_dir_all(&storage).unwrap();
            if corrupt {
                fs::write(storage.join("run.json"), "corrupt").unwrap();
            } else {
                let record: RunRecord = serde_json::from_value(json!({
                    "schema_version":1,"run_id":"staged","session_id":name,"agent":"fixture",
                    "pid":0,"command":[],"state":"completed","started_at_unix_ms":10,
                    "storage":storage,"network":{}
                }))
                .unwrap();
                record.write().unwrap();
            }
        }
        let output = f.named("apply", &["--all", "--json"]);
        object(&output, 1);
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains(if corrupt {
                "corrupt default Job"
            } else {
                "ambiguous existing Job"
            }),
            "{output:?}"
        );
        object(
            &f.named(
                "apply",
                &["--mount", "/workspace-0", "--all", "--dry-run", "--json"],
            ),
            0,
        );
        assert!(!f.sources[0].join("result").exists());
    }
}

#[test]
fn ordinary_mounts_and_unstarted_containers_are_not_stages() {
    for ordinary in [true, false] {
        let mut f = Fixture::new(1);
        if ordinary {
            f.instance["config"]["mounts"][0]["staged"] = json!(false);
        } else {
            f.instance["generation"] = json!("");
        }
        f.save();
        for mount in [None, Some("/workspace-0")] {
            let mut args = vec!["--all", "--json"];
            if let Some(mount) = mount {
                args.extend(["--mount", mount]);
            }
            object(&f.named("apply", &args), 1);
            assert!(f.stages[0].join("upper/result").exists());
            assert!(!f.sources[0].join("result").exists());
        }
    }
}

#[test]
fn interactive_container_apply_can_go_back_and_accept_the_selected_mount() {
    let script = r#"
import errno, os, pty, signal, sys
binary, root, store, read_size = sys.argv[1:]
read_size = int(read_size)
pid, master = pty.fork()
if pid == 0:
    os.chdir(root)
    os.environ.update(HOME=root, PVISOR_RUN_HOME=root+'/runs', PVISOR_DOCKER_HOME=store)
    os.execv(binary, [binary, 'apply', 'staged', '--mount', '/workspace-1'])
pending = bytearray()
transcript = bytearray()

def timed_out(signum, frame):
    raise AssertionError('picker timed out: '+repr(transcript))

def read_chunk():
    try:
        chunk = os.read(master, read_size)
    except OSError as error:
        if error.errno != errno.EIO: raise
        return b''  # Linux PTYs report slave closure as EIO.
    transcript.extend(chunk)
    assert len(transcript) < 200000, transcript
    return chunk

def read_until(marker):
    # PTY reads can split any line or coalesce several screens. Consume only
    # this frame, retaining every trailing byte for the next expectation.
    while marker not in pending:
        chunk = read_chunk()
        assert chunk, ('terminal closed before', marker, transcript)
        pending.extend(chunk)
    end = pending.index(marker) + len(marker)
    frame = bytes(pending[:end])
    del pending[:end]
    return frame

def picker(checked):
    read_until(b'Enter preview, q quit\r\n')
    row = read_until(b'\r\n')
    assert b'result' in row, (row, transcript)
    assert (b'> [x]' if checked else b'> [ ]') in row, (row, transcript)

signal.signal(signal.SIGALRM, timed_out)
signal.setitimer(signal.ITIMER_REAL, 15)
reaped = False
try:
    for attempt in range(2):
        picker(checked=attempt != 0)
        os.write(master, b'a')
        # Selecting all redraws the picker; wait for its checked row before
        # Enter, rather than treating the header as a complete screen.
        picker(checked=True)
        os.write(master, b'\r')
        read_until(b'[a] apply  [b] back  [d] diff  [q] quit\r\n')
        os.write(master, b'b' if attempt == 0 else b'a')
    while read_chunk(): pass
    _, status = os.waitpid(pid, 0)
    reaped = True
    assert status == 0, (status, transcript)
except BaseException:
    if not reaped:
        try: os.kill(pid, signal.SIGKILL)
        except ProcessLookupError: pass
        os.waitpid(pid, 0)
    raise
finally:
    signal.setitimer(signal.ITIMER_REAL, 0)
    os.close(master)
"#;
    // Exercise bytewise and short reads as well as ordinary coalesced reads.
    for read_size in [1, 7, 65536] {
        let f = Fixture::new(2);
        let output = Command::new("python3")
            .args(["-c", script, env!("CARGO_BIN_EXE_pvisor")])
            .arg(f.root.path())
            .arg(&f.store)
            .arg(read_size.to_string())
            .output()
            .unwrap();
        success(&output);
        assert_eq!(
            fs::read_to_string(f.sources[1].join("result")).unwrap(),
            "result-1"
        );
        assert!(!f.sources[0].join("result").exists());
        assert!(f.stages[0].join("upper/result").exists());
    }
}

#[test]
fn corrupt_job_index_never_falls_back_to_container() {
    let f = Fixture::new(1);
    let storage = f.root.path().join("jobs");
    let index = storage.join(".pvisor/runs");
    fs::create_dir_all(&index).unwrap();
    fs::write(index.join("737461676564.json"), "corrupt").unwrap();
    for operation in ["review", "apply", "drop"] {
        let option = if operation == "apply" {
            "--store"
        } else {
            "--output-dir"
        };
        let mut args = vec![option, storage.to_str().unwrap()];
        if operation == "apply" {
            args.extend(["--all", "--json"]);
        }
        let output = f.named(operation, &args);
        assert!(!output.status.success(), "{output:?}");
        if operation == "apply" {
            object(&output, 1);
        }
        assert!(f.stages[0].join("upper/result").exists());
        assert!(!f.sources[0].join("result").exists());
    }
}
