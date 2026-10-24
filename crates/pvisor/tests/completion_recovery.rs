//! Kill the owner before the receipt and at both terminal publication boundaries.

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

struct StopAtPublication {
    root: PathBuf,
    journal: Journal,
    phase: String,
}

#[async_trait]
impl EventSink for StopAtPublication {
    async fn append(&self, event: &Event) -> anyhow::Result<Receipt> {
        if event.name() == self.phase {
            assert!(RunBundle::path(&self.root.join("stage")).is_file());
            if self.phase == "network.observed" {
                assert!(!self.root.join("stage/completion.json").exists());
            } else {
                assert_eq!(
                    RunBundle::read(&self.root.join("stage"))?.run.state,
                    pvisor_core::RunState::Completed
                );
            }
            std::fs::write(self.root.join("prepared"), b"boundary reached")?;
            std::future::pending::<()>().await;
        }
        let receipt = JournalStore::append(&self.journal, event.clone())?;
        if matches!(event.name(), "completed" | "run.completed" | "run.failed") {
            use std::io::Write;
            writeln!(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.root.join("notifications"))?,
                "{}",
                event.name()
            )?;
        }
        Ok(receipt)
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
                .network(pvisor::NetworkDriverConfig::new(
                    pvisor::OverlayNetMode::Proxy,
                    pvisor_overlaynet::NetworkConfig {
                        mode: pvisor_overlaynet::NetworkMode::NoNetwork,
                        ..Default::default()
                    },
                ))
                .event_sink(Arc::new(StopAtPublication {
                    root: root.clone(),
                    journal: Journal::memory(),
                    phase: std::env::var("PVISOR_COMPLETION_CRASH_PHASE").unwrap(),
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
fn killed_owner_reconciles_only_receipts_across_terminal_publication_windows() {
    for phase in ["network.observed", "completed", "run.completed"] {
        let root = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "completion_crash_child", "--nocapture"])
            .env("PVISOR_COMPLETION_CRASH_TEST", root.path())
            .env("PVISOR_COMPLETION_CRASH_PHASE", phase)
            .env("PVISOR_STARTUP_TIMING", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(root.path().join("child.log")).unwrap())
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
        assert!(
            prepared,
            "child did not reach {phase}: {}",
            std::fs::read_to_string(root.path().join("child.log")).unwrap()
        );
        let stage = root.path().join("stage");
        assert!(RunBundle::path(&stage).is_file());
        assert_eq!(
            stage.join("completion.json").exists(),
            phase != "network.observed"
        );
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
        let notifications =
            std::fs::read_to_string(root.path().join("notifications")).unwrap_or_default();
        if phase == "network.observed" {
            assert_eq!(status.record.state, RunRecordState::Unknown);
            assert!(status.record.require_stopped().is_err());
            assert!(RunBundle::read(&stage).is_err());
            assert!(notifications.is_empty());
        } else {
            assert_eq!(status.record.state, RunRecordState::Completed);
            status.record.require_stopped().unwrap();
            assert_eq!(
                RunBundle::read(&stage).unwrap().run.state,
                pvisor_core::RunState::Completed
            );
            assert_eq!(
                notifications,
                if phase == "run.completed" {
                    "completed\n"
                } else {
                    ""
                }
            );
        }
    }
}
