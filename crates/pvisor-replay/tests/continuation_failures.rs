use pvisor_replay::{AgentKind, AgentStatus, PlaybackRequest, ReplayMode, execute};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn opencode_continuation_preserves_failure_and_step_budget() {
    if let Ok(root) = std::env::var("PVISOR_REPLAY_TEST_ROOT") {
        let root = std::path::PathBuf::from(root);
        let entry = root.join("opencode");
        fs::write(&entry,r#"#!/usr/bin/env python3
import json, os, sys, urllib.request
if sys.argv[1]=='--version':
    print('1.17.7'); sys.exit(0)
if sys.argv[1]=='import': sys.exit(0)
url=os.environ['OPENAI_BASE_URL'].rstrip('/')+'/responses'
payload=json.dumps({'model':'audit','input':[{'role':'user','content':'audit continuation'}]}).encode()
request=urllib.request.Request(url,data=payload,headers={'Content-Type':'application/json','Authorization':'Bearer '+os.environ['OPENAI_API_KEY']})
with urllib.request.urlopen(request,timeout=5) as response: response.read()
print(json.dumps({'type':'step_start','sessionID':'audit'}),flush=True)
print(json.dumps({'type':'step_finish','sessionID':'audit','part':{'reason':'tool-calls'}}),flush=True)
if os.environ['PVISOR_REPLAY_TEST_CASE']=='budget':
    import time
    print('message=loop step=1',file=sys.stderr,flush=True)
    time.sleep(10)
else:
    print('fatal: synthetic failure after one complete turn',file=sys.stderr,flush=True)
    sys.exit(42)
"#).unwrap();
        fs::set_permissions(&entry, fs::Permissions::from_mode(0o700)).unwrap();
        let trajectory = root.join("trajectory.jsonl");
        let events = [
            serde_json::json!({"type":"user","sessionID":"audit","parts":[{"type":"text","text":"audit"}]}),
            serde_json::json!({"type":"step_start","sessionID":"audit"}),
            serde_json::json!({"type":"tool_use","sessionID":"audit","part":{"type":"tool","tool":"bash","callID":"call-1","state":{"status":"completed","input":{"command":"printf fresh"},"output":"fresh"}}}),
            serde_json::json!({"type":"step_finish","sessionID":"audit","part":{"reason":"tool-calls"}}),
        ];
        fs::write(
            &trajectory,
            events
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        let report = execute(PlaybackRequest {
            agent: AgentKind::Opencode,
            trajectory,
            after_step: 1,
            workspace: root.join("workspace"),
            state_dir: root.join("state"),
            output_dir: root.join("output"),
            agent_entrypoint: Some(entry),
            agent_runtime: None,
            disallowed_tools: vec![],
            trajectory_assets: None,
            session_id: None,
            max_steps: Some(2),
            mode: ReplayMode::ReplayAndContinue,
            allow_stale_observations: false,
            run_id: Some("audit".into()),
            disable_thinking: false,
            boundary_user_prompt: Some("audit continuation".into()),
        })
        .unwrap();
        if std::env::var("PVISOR_REPLAY_TEST_CASE").unwrap() == "budget" {
            assert_eq!(report.exit_code, 0, "{:#?}", report.result);
            assert_eq!(report.result.agent_status, AgentStatus::MaxSteps);
        } else {
            assert_eq!(report.exit_code, 40, "{:#?}", report.result);
            assert_eq!(
                report.result.failure.unwrap().category,
                "continuation_error"
            );
            assert!(
                fs::read_to_string(report.result.output_dir.join("logs/opencode-events.jsonl"))
                    .unwrap()
                    .contains("step_finish")
            );
        }
        return;
    }
    for case in ["failure", "budget"] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        fs::create_dir(root.join("workspace")).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let supplier = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline);
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut data = Vec::new();
            loop {
                let mut buffer = [0; 4096];
                let n = stream.read(&mut buffer).unwrap();
                assert!(n > 0);
                data.extend_from_slice(&buffer[..n]);
                if let Some(index) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&data[..index]).to_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .unwrap()
                        .trim()
                        .parse::<usize>()
                        .unwrap();
                    if data.len() >= index + 4 + length {
                        break;
                    }
                }
            }
            let body = r#"{"id":"audit","output":[]}"#;
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        });
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "opencode_continuation_preserves_failure_and_step_budget",
                "--nocapture",
            ])
            .env("PVISOR_REPLAY_TEST_ROOT", root)
            .env("PVISOR_REPLAY_TEST_CASE", case)
            .env("OPENAI_BASE_URL", format!("http://{address}/v1"))
            .env("OPENAI_API_KEY", "fixture")
            .env("MODEL_NAME", "openai/audit")
            .output()
            .unwrap();
        supplier.join().unwrap();
        assert!(
            output.status.success(),
            "case={case}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
