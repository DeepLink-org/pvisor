//! Kill the owning process after Bundle preparation, before completion commit.

use async_trait::async_trait;
use pvisor::job_service::{JobSelection, RuntimeJobService, ServiceContext, StatusRequest};
use pvisor::{EventSink, PVisor, RunBundle, RunRecordState};
use pvisor_core::RunSpec;
use pvisor_core::event::{Event, Receipt};
use pvisor_journal::api::{Journal, JournalStore};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct StopBeforeCommit {
    root: PathBuf,
    journal: Journal,
}

#[async_trait]
impl EventSink for StopBeforeCommit {
    async fn append(&self, event: &Event) -> anyhow::Result<Receipt> {
        if event.name() == "run.completed" {
            std::fs::write(self.root.join("prepared"), b"bundle prepared")?;
            std::future::pending::<()>().await;
        }
        Ok(JournalStore::append(&self.journal, event.clone())?)
    }
}

#[test]
fn completion_crash_child() {
    let Some(root) = std::env::var_os("PVISOR_COMPLETION_CRASH_TEST") else {
        return;
    };
    let root = PathBuf::from(root);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let runtime = PVisor::builder()
                .storage(root.join("stage"))
                .instance_control(false)
                .event_sink(Arc::new(StopBeforeCommit {
                    root: root.clone(),
                    journal: Journal::memory(),
                }))
                .build();
            let handle = runtime
                .run(RunSpec::process("crash-window", "test", "/usr/bin/true"))
                .await
                .unwrap();
            std::fs::write(
                root.join("agentctl-path"),
                handle.agentctl().endpoint().as_os_str().as_encoded_bytes(),
            )
            .unwrap();
            handle.wait().await.unwrap();
        });
}

#[test]
fn killed_owner_never_promotes_a_prepared_bundle_to_completed() {
    let root = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "completion_crash_child", "--nocapture"])
        .env("PVISOR_COMPLETION_CRASH_TEST", root.path())
        .env("PVISOR_STARTUP_TIMING", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !(root.path().join("prepared").exists() && root.path().join("agentctl-path").exists())
        && Instant::now() < deadline
        && child.try_wait().ok().flatten().is_none()
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    let prepared = root.path().join("prepared").exists();
    let _ = child.kill();
    assert!(!child.wait().unwrap().success());
    if let Ok(socket) = std::fs::read_to_string(root.path().join("agentctl-path")) {
        let _ = std::fs::remove_file(socket);
    }
    assert!(prepared, "child did not reach the intended crash window");
    let stage = root.path().join("stage");
    assert!(RunBundle::path(&stage).is_file());
    assert!(!stage.join("completion.json").exists());
    let status = RuntimeJobService::status(
        &ServiceContext::default(),
        StatusRequest {
            job: JobSelection {
                selector: Some(stage.clone()),
                storage: stage.clone(),
            },
        },
    )
    .unwrap();
    assert_eq!(status.record.state, RunRecordState::Unknown);
    assert!(status.record.require_stopped().is_err());
    assert!(RunBundle::read(&stage).is_err());
}
