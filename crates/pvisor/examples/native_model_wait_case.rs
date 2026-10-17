//! EXP-003 M0 correctness driver: real guest -> Gateway -> gated local supplier.
//! This checks lifecycle semantics; it is not a performance benchmark.
use anyhow::{Context, ensure};
use pvisor::{
    GatewayDriverConfig, MemoryEventSink, NativeModelWait, OverlayHint, PVisor, VmExecutor,
    VmSettings,
};
use pvisor_core::{RunInvocation, RunSpec, RunState, StdioMode};
use pvisor_gateway::config::{ModelRoute, ProxyConfig};
use serde_json::json;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::Notify;

const GUEST: &str = r#"
import hashlib, http.client, json, os, urllib.parse
data = bytearray(bytes(range(256)) * 32768)
digest = hashlib.sha256(data).hexdigest()
url = urllib.parse.urlsplit(os.environ['OPENAI_BASE_URL'])
conn = http.client.HTTPConnection(url.hostname, url.port, timeout=120)
headers = {'Content-Type': 'application/json',
           'Authorization': 'Bearer ' + os.environ['OPENAI_API_KEY'],
           'x-pvisor-session-id': os.environ['PVISOR_RUN_ID'],
           'x-pvisor-inference-idle': 'true'}
conn.request('POST', url.path + '/chat/completions',
             json.dumps({'model': 'test-model', 'messages': [{'role':'user','content':'hello'}]}), headers)
response = conn.getresponse()
body = response.read()
assert response.status == 200, (response.status, body)
assert json.loads(body)['choices'][0]['message']['content'] == 'iocp-ok', body
assert hashlib.sha256(data).hexdigest() == digest, 'guest RAM changed'
conn.close()
print('native-model-wait-guest-ok', flush=True)
"#;

fn main() -> anyhow::Result<()> {
    if pvisor::run_krun_internal_if_requested()? {
        return Ok(());
    }
    tokio::runtime::Runtime::new()?.block_on(run())
}

async fn run() -> anyhow::Result<()> {
    let mode = std::env::args()
        .nth(1)
        .context("expected running, pause, offload, manual or cancel")?;
    ensure!(
        ["running", "pause", "offload", "manual", "cancel"].contains(&mode.as_str()),
        "unknown mode"
    );
    let root = PathBuf::from(
        std::env::args()
            .nth(2)
            .context("expected fresh output directory")?,
    );
    std::fs::create_dir(&root)?;
    let root = root.canonicalize()?;
    let workspace = root.join("workspace");
    std::fs::create_dir(&workspace)?;
    let received = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let upstream = listener.local_addr()?;
    let app = axum::Router::new().fallback({
        let received = received.clone();
        let release = release.clone();
        move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<serde_json::Value>| {
            let received = received.clone();
            let release = release.clone();
            async move {
                assert!(!headers.contains_key("x-pvisor-inference-idle"));
                assert_eq!(body["model"], "test-model");
                received.notify_one();
                release.notified().await;
                axum::Json(json!({"id":"reply", "object":"chat.completion", "model":"test-model",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"iocp-ok"},"finish_reason":"stop"}]}))
            }
        }
    });
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let proxy = ProxyConfig {
        listen: "127.0.0.1:0".into(),
        admin_listen: "127.0.0.1:0".into(),
        agent_id: "iocp-case".into(),
        session_header: "x-pvisor-session-id".into(),
        capture_level: Default::default(),
        debug: false,
        network: Default::default(),
        overlay: Default::default(),
        models: vec![ModelRoute {
            name: "test-model".into(),
            provider: Some("openai".into()),
            upstream: Some(format!("http://{upstream}/v1")),
            api_key: Some("local-test-key".into()),
            api_key_env: None,
            upstream_anthropic: None,
            forward: None,
        }],
    };
    let mut gateway = GatewayDriverConfig::new(proxy).output_dir(root.join("capture"));
    if mode != "running" {
        gateway = gateway.native_model_wait(if mode == "pause" {
            NativeModelWait::Pause
        } else {
            NativeModelWait::Offload
        });
    }
    let events = Arc::new(MemoryEventSink::default());
    let runtime = PVisor::builder()
        .executors(vec![Arc::new(VmExecutor::new(VmSettings {
            rootfs: Some(
                std::env::var_os("PVISOR_CASE_ROOTFS")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| "/".into()),
            ),
            library_dir: std::env::var_os("PVISOR_CASE_VM_LIBRARY_DIR").map(PathBuf::from),
            ram_backing: Some(root.join("ram")),
            memory_mib: 256,
            cpus: 1,
            ..Default::default()
        })?)])
        .gateway(gateway)
        .storage(root.join("run"))
        .event_sink(events.clone())
        .overlay(OverlayHint {
            lower_dirs: vec![workspace.clone()],
            stage_dir: Some(root.join("stage")),
            ..Default::default()
        })
        .build();
    let mut spec = RunSpec::process(
        "native-model-wait-case",
        "iocp-case",
        std::env::var("PVISOR_CASE_VM_PYTHON").unwrap_or_else(|_| "/usr/bin/python3".into()),
    );
    spec.runtime.timeout_ms = Some(120_000);
    let RunInvocation::Process(process) = &mut spec.invocation;
    process.args = vec!["-c".into(), GUEST.into()];
    process.cwd = Some("/workspace".into());
    process.stdout = StdioMode::Capture;
    process.stderr = StdioMode::Capture;
    spec.metadata
        .insert("pvisor.workspace".into(), json!(workspace));
    spec.metadata
        .insert("pvisor.vm.overlay_target".into(), json!("/workspace"));
    spec.metadata
        .insert("pvisor.vm.guest_cwd".into(), json!("/workspace"));
    let mut handle = runtime.run(spec).await?;
    let cancellation = handle.cancellation();
    let check = async {
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                ensure!(
                    !handle.status().state.is_terminal(),
                    "guest exited before upstream dispatch: {:?}",
                    handle.status()
                );
                tokio::select! {
                    _ = received.notified() => break,
                    _ = handle.status_changed() => {}
                }
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("supplier did not receive request")??;
        ensure!(
            handle.status().state
                == if mode == "running" {
                    RunState::Running
                } else {
                    RunState::Suspended
                },
            "incorrect state at upstream dispatch: {:?}",
            handle.status()
        );
        if mode == "manual" {
            handle.pause().await?;
            release.notify_one();
            tokio::time::sleep(Duration::from_millis(200)).await;
            ensure!(
                handle.status().state == RunState::Suspended,
                "completion overrode manual pause"
            );
            handle.resume().await?;
        } else {
            if mode == "cancel" {
                handle.cancel();
            }
            release.notify_one();
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if check.is_err() {
        cancellation.cancel();
        release.notify_one();
    }
    let result = handle.wait().await?;
    server.abort();
    let observations = events
        .events()
        .into_iter()
        .filter(|e| e.name().starts_with("vm.control_"))
        .collect::<Vec<_>>();
    std::fs::write(
        root.join("result.json"),
        serde_json::to_vec_pretty(&json!({"mode":mode,"result":result,"controls":observations}))?,
    )?;
    check.with_context(|| format!("guest stderr: {:?}", result.output.stderr))?;
    if mode == "cancel" {
        ensure!(
            result.state == RunState::Cancelled,
            "expected cancellation: {result:?}"
        );
    } else {
        ensure!(
            result.state == RunState::Completed
                && result.exit_code == Some(0)
                && result
                    .output
                    .stdout
                    .as_deref()
                    .is_some_and(|s| s.contains("native-model-wait-guest-ok")),
            "guest failed: {result:?}"
        );
    }
    let completed = observations
        .iter()
        .filter(|e| e.name() == "vm.control_completed")
        .filter_map(|e| e.observation_payload())
        .filter(|p| p["model_wait_epoch"].is_u64())
        .collect::<Vec<_>>();
    ensure!(
        completed.len()
            == match mode.as_str() {
                "running" => 0,
                "manual" | "cancel" => 1,
                _ => 2,
            },
        "unexpected automatic transitions: {completed:?}"
    );
    if ["offload", "manual", "cancel"].contains(&mode.as_str()) {
        ensure!(
            completed[0]["value"]["vm"]["state"] == "offloaded",
            "missing offload ACK"
        );
        ensure!(
            completed[0]["value"]["vm"]["memory"]["backed_bytes"]
                .as_u64()
                .unwrap_or(0)
                > 0,
            "missing RAM report"
        );
    }
    println!("native-model-wait-case-ok {mode}");
    Ok(())
}
