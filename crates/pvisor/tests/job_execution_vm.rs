//! Native Job CLI acceptance. No legacy snapshot frontend or substitute runner.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

fn command() -> Command {
    // Native compatibility binds the executable contents. Keep this test's
    // binary immutable while unrelated builds may replace target/debug/pvisor.
    static BINARY: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let directory = BINARY.get_or_init(|| {
        let directory = tempfile::tempdir().unwrap();
        fs::copy(
            env!("CARGO_BIN_EXE_pvisor"),
            directory.path().join("pvisor"),
        )
        .unwrap();
        directory
    });
    let mut command = Command::new(directory.path().join("pvisor"));
    command.env("PVISOR_STARTUP_TIMING", "0");
    command
}
fn invoke(stage: &Path, args: &[&str]) -> Output {
    let mut cmd = command();
    let offset = if args[0] == "checkpoint" { 2 } else { 1 };
    cmd.args(&args[..offset]).arg(stage).args(&args[offset..]);
    cmd.output().unwrap()
}
fn json(output: Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
struct Running {
    child: Child,
    stage: PathBuf,
}
impl Drop for Running {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_some() {
            return;
        }
        let _ = invoke(&self.stage, &["kill"]);
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn wait_for<T>(mut action: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(value) = action() {
            return value;
        }
        assert!(Instant::now() < deadline, "native Job acceptance deadline");
        std::thread::sleep(Duration::from_millis(25));
    }
}
fn counter(stage: &Path) -> Option<u64> {
    let record = pvisor::RunRecord::read(stage).ok()?;
    let upper = &record.overlay.as_ref()?.upper.upper_dir;
    fs::read_to_string(upper.join("counter"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[test]
#[ignore = "real KVM and /dev/fuse; static C compiler; PVISOR_TEST_LIBRARY_DIR for GNU firmware"]
fn native_job_capture_suspend_resume_and_execution_fork_preserve_process_and_files() {
    acceptance(false, false);
}
#[test]
#[ignore = "real KVM and /dev/fuse; static C compiler; PVISOR_TEST_LIBRARY_DIR for GNU firmware"]
fn native_job_nested_stage_preserves_guest_projection() {
    acceptance(true, false);
}
#[test]
#[ignore = "real KVM and /dev/fuse; static C compiler; PVISOR_TEST_LIBRARY_DIR for GNU firmware"]
fn native_job_nested_stage_chunked_filesystems_preserve_guest_projection() {
    acceptance(true, true);
}

fn acceptance(nested: bool, pooled: bool) {
    let temp = tempfile::tempdir().unwrap();
    let rootfs = temp.path().join("rootfs");
    for directory in ["bin", "dev", "proc", "tmp"] {
        fs::create_dir_all(rootfs.join(directory)).unwrap();
    }
    let guest = temp.path().join("guest.c");
    fs::write(
        &guest,
        r#"
#include <stdio.h>
#include <fcntl.h>
#include <unistd.h>
#include <time.h>
int main(void) {
    /* A restart fails; only saved CPU/RAM and the already open descriptor can continue. */
    int started=open("started-once",O_CREAT|O_EXCL|O_WRONLY,0644);
    if(started<0) return 88;
    close(started);
    int fd=open("counter",O_CREAT|O_RDWR,0644);
    if(fd<0) return 89;
    unsigned long counter=1000;
    for(;;) {
        char data[64];int size=snprintf(data,sizeof(data),"%lu\n",counter++);
        if(pwrite(fd,data,size,0)!=size || ftruncate(fd,size)) return 90;
        struct timespec delay={0,100000000};nanosleep(&delay,0);
    }
}
"#,
    )
    .unwrap();
    assert!(
        Command::new("cc")
            .args(["-O2", "-static"])
            .arg(&guest)
            .arg("-o")
            .arg(rootfs.join("bin/probe"))
            .status()
            .unwrap()
            .success()
    );
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let stage = if nested {
        workspace.join("stage/job")
    } else {
        temp.path().join("job")
    };
    let log = temp.path().join("source.log");
    let mut cmd = command();
    if pooled {
        let config = temp.path().join("pool.toml");
        fs::write(
            &config,
            format!(
                "[vm]\nsnapshot_filesystem_pool = {:?}\n",
                temp.path().join("pool")
            ),
        )
        .unwrap();
        cmd.arg("run").arg("--config").arg(config);
    }
    if !pooled {
        cmd.arg("run");
    }
    cmd.current_dir(&workspace)
        .env("JOB_CAPTURE_ENV", "original")
        .args([
            "--executor",
            "vm",
            "--overlaynet",
            "off",
            "--memory",
            "128MiB",
            "--stdio",
            "capture",
            "--rootfs",
        ])
        .arg(&rootfs)
        .arg("--stage")
        .arg(&stage);
    if let Some(directory) = std::env::var_os("PVISOR_TEST_LIBRARY_DIR") {
        cmd.arg("--vm-library-dir").arg(directory);
    }
    cmd.args(["--", "/bin/probe"])
        .stdout(Stdio::null())
        .stderr(fs::File::create(&log).unwrap());
    let mut source = Running {
        child: cmd.spawn().unwrap(),
        stage: stage.clone(),
    };
    let first = wait_for(|| {
        if let Some(status) = source.child.try_wait().unwrap() {
            panic!(
                "source exited {status}: {}",
                fs::read_to_string(&log).unwrap()
            );
        }
        counter(&stage)
    });
    assert!(first >= 1000);
    let original = pvisor::RunRecord::read(&stage).unwrap();
    let cp = json(invoke(
        &stage,
        &[
            "checkpoint",
            "create",
            "--kind",
            "execution",
            "--ram-storage",
            "compressed",
            "--request-id",
            "capture-one",
            "--json",
        ],
    ));
    let id = cp["checkpoint_id"].as_str().unwrap();
    assert_eq!(cp["checkpoint"]["ram_storage"], "compressed");
    wait_for(|| counter(&stage).filter(|current| *current > first));
    let replay = json(invoke(
        &stage,
        &[
            "checkpoint",
            "create",
            "--kind",
            "execution",
            "--ram-storage",
            "compressed",
            "--request-id",
            "capture-one",
            "--json",
        ],
    ));
    assert_eq!(replay["checkpoint_id"], id);
    assert!(
        !invoke(
            &stage,
            &[
                "checkpoint",
                "create",
                "--kind",
                "execution",
                "--ram-storage",
                "raw",
                "--request-id",
                "capture-one"
            ]
        )
        .status
        .success()
    );
    assert_eq!(
        json(invoke(&stage, &["checkpoint", "verify", id, "--json"]))["verified"],
        true
    );

    let child_stage = temp.path().join("branch");
    let child_log = temp.path().join("branch.log");
    let child = command()
        .arg("fork")
        .arg(&stage)
        .args([
            "--state",
            "execution",
            "--checkpoint",
            id,
            "--request-id",
            "branch-one",
            "--eager-ram",
            "--stage",
        ])
        .arg(&child_stage)
        .stdout(Stdio::null())
        .stderr(fs::File::create(&child_log).unwrap())
        .spawn()
        .unwrap();
    let mut branch = Running {
        child,
        stage: child_stage.clone(),
    };
    wait_for(|| {
        if let Some(status) = branch.child.try_wait().unwrap() {
            panic!(
                "branch exited {status}: {}",
                fs::read_to_string(&child_log).unwrap()
            );
        }
        counter(&child_stage)
    });
    let child_record = pvisor::RunRecord::read(&child_stage).unwrap();
    assert_ne!(child_record.run_id, original.run_id);
    assert_eq!(child_record.lineage.as_ref().unwrap().checkpoint_id, id);
    assert_ne!(
        child_record.overlay.as_ref().unwrap().upper.upper_dir,
        original.overlay.as_ref().unwrap().upper.upper_dir
    );
    assert!(source.child.try_wait().unwrap().is_none());
    assert!(
        invoke(
            &stage,
            &[
                "fork",
                "--state",
                "execution",
                "--checkpoint",
                id,
                "--stage",
                child_stage.to_str().unwrap(),
                "--request-id",
                "branch-one",
                "--eager-ram"
            ]
        )
        .status
        .success()
    );
    assert!(
        !invoke(&stage, &["checkpoint", "delete", id])
            .status
            .success()
    );

    let suspended = json(invoke(
        &stage,
        &[
            "suspend",
            "--ram-storage",
            "raw",
            "--request-id",
            "suspend-one",
            "--json",
        ],
    ));
    assert_eq!(suspended["state"], "suspended");
    assert_eq!(suspended["checkpoint"]["ram_storage"], "raw");
    let frozen = counter(&stage).unwrap();
    let status = wait_for(|| source.child.try_wait().unwrap());
    assert!(status.success(), "{}", fs::read_to_string(&log).unwrap());
    assert_eq!(json(invoke(&stage, &["status", "--json"]))["live"], false);
    assert!(!invoke(&stage, &["drop"]).status.success());
    assert!(!invoke(&stage, &["apply"]).status.success());
    let head = suspended["checkpoint_id"].as_str().unwrap();
    assert!(
        !invoke(&stage, &["checkpoint", "delete", head])
            .status
            .success()
    );
    assert_eq!(
        json(invoke(&stage, &["suspend", "--json"]))["checkpoint_id"],
        head
    );
    // Restore reads the owned object; it must not need the launch rootfs.
    fs::remove_dir_all(&rootfs).unwrap();

    let resume_log = temp.path().join("resume.log");
    let resumed = command()
        .arg("resume")
        .arg(&stage)
        .args(["--request-id", "resume-one"])
        .env("JOB_CAPTURE_ENV", "changed")
        .stdout(Stdio::null())
        .stderr(fs::File::create(&resume_log).unwrap())
        .spawn()
        .unwrap();
    source = Running {
        child: resumed,
        stage: stage.clone(),
    };
    wait_for(|| {
        if let Some(status) = source.child.try_wait().unwrap() {
            panic!(
                "resume exited {status}: {}",
                fs::read_to_string(&resume_log).unwrap()
            );
        }
        counter(&stage).filter(|current| *current > frozen)
    });
    let active = pvisor::RunRecord::read(&stage).unwrap();
    assert!(
        source.child.try_wait().unwrap().is_none(),
        "{}",
        fs::read_to_string(&resume_log).unwrap()
    );
    assert_eq!(active.run_id, original.run_id);
    assert_ne!(active.attempt_id, original.attempt_id);
    assert_ne!(active.stage_dir(), original.stage_dir());
    assert!(original.stage_dir().join("run-bundle.json").exists());
    assert!(
        invoke(&stage, &["resume", "--request-id", "resume-one"])
            .status
            .success()
    );
    assert!(
        !invoke(&stage, &["resume", "--request-id", "another"])
            .status
            .success()
    );
    let advanced = json(invoke(
        &stage,
        &[
            "checkpoint",
            "create",
            "--kind",
            "execution",
            "--ram-storage",
            "compressed",
            "--request-id",
            "capture-resumed",
            "--json",
        ],
    ));
    assert_eq!(
        advanced["checkpoint"]["source_attempt_id"],
        active.attempt_id.unwrap()
    );
    assert_eq!(
        json(invoke(
            &stage,
            &["checkpoint", "list", "--kind", "execution", "--json"]
        ))["checkpoints"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let unreferenced = advanced["checkpoint_id"].as_str().unwrap();
    assert!(
        invoke(&stage, &["checkpoint", "delete", unreferenced])
            .status
            .success()
    );
    // The two saved machines retain independent upper files and private RAM.
    json(invoke(
        &child_stage,
        &["suspend", "--request-id", "branch-stop", "--json"],
    ));
    wait_for(|| branch.child.try_wait().unwrap());
    let branch_frozen = counter(&child_stage).unwrap();
    let source_before = counter(&stage).unwrap();
    wait_for(|| counter(&stage).filter(|current| *current > source_before));
    assert_eq!(counter(&child_stage), Some(branch_frozen));
    assert!(invoke(&child_stage, &["kill"]).status.success());
    assert!(!invoke(&child_stage, &["resume"]).status.success());
    assert!(!workspace.join("counter").exists());
    fs::write(workspace.join("counter"), b"external change").unwrap();
    assert!(!invoke(&child_stage, &["apply"]).status.success());
    assert_eq!(
        fs::read(workspace.join("counter")).unwrap(),
        b"external change"
    );
    assert!(invoke(&child_stage, &["drop"]).status.success());
}
