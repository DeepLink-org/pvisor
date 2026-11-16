#![cfg(unix)]

use pvisor_replay::{AgentKind, PlaybackRequest, ReplayMode, execute};
use serde_json::{Value, json};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn failing_pi_runtime(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    let node_bin = root.join("node/bin");
    let package = root.join("npm-global/lib/node_modules/@earendil-works/pi-coding-agent");
    for dir in [&bin, &node_bin, &package.join("dist")] {
        fs::create_dir_all(dir).unwrap();
    }
    let entry = bin.join("pi");
    fs::write(&entry, "#!/bin/sh\nprintf '0.83.0\\n'\n").unwrap();
    fs::set_permissions(&entry, fs::Permissions::from_mode(0o755)).unwrap();
    let node = Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    assert!(node.status.success());
    symlink(
        String::from_utf8(node.stdout).unwrap().trim(),
        node_bin.join("node"),
    )
    .unwrap();
    fs::write(package.join("package.json"),r#"{"name":"@earendil-works/pi-coding-agent","type":"module","exports":{".":{"import":"./dist/index.mjs"}}}"#).unwrap();
    fs::write(
        package.join("dist/index.mjs"),
        r#"
import fs from 'node:fs';
export function createCodingTools(){ return [{name:'bash',async execute(_id,args){
  fs.appendFileSync(args.marker,'effect\n');
  process.exit(23);
}}]; }
export const SessionManager = {create(){ return {appendMessage(){}}; }};
"#,
    )
    .unwrap();
    entry
}
#[test]
fn interrupted_sdk_replay_blocks_uncertain_effect_with_fresh_output_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let workspace = root.join("workspace");
    fs::create_dir(&workspace).unwrap();
    let marker = workspace.join("effects");
    let trajectory = root.join("pi.jsonl");
    let events = [
        json!({"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"test"}],"timestamp":1}}),
        json!({"type":"turn_end","message":{"role":"assistant","provider":"sweeval","model":"fake-model","api":"openai-completions","content":[{"type":"toolCall","id":"call-1","name":"bash","arguments":{"marker":marker}}],"stopReason":"toolUse","usage":{"input":1,"output":1},"timestamp":2},"toolResults":[{"role":"toolResult","toolCallId":"call-1","toolName":"bash","content":[{"type":"text","text":"old"}],"isError":false,"timestamp":3}]}),
    ];
    fs::write(
        &trajectory,
        events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    let entry = failing_pi_runtime(&root.join("pi-runtime"));
    let mut request = PlaybackRequest {
        agent: AgentKind::PiAgent,
        trajectory,
        after_step: 1,
        workspace,
        state_dir: root.join("state"),
        output_dir: root.join("output-1"),
        agent_entrypoint: Some(entry),
        agent_runtime: None,
        disallowed_tools: vec![],
        trajectory_assets: None,
        session_id: None,
        max_steps: Some(1),
        mode: ReplayMode::ReplayOnly,
        allow_stale_observations: false,
        run_id: Some("same-run".into()),
        disable_thinking: false,
        boundary_user_prompt: None,
    };
    let mut preparation = request.clone();
    preparation.mode = ReplayMode::PrepareOnly;
    preparation.run_id = Some("prepare-only".into());
    assert_eq!(execute(preparation).unwrap().exit_code, 0);
    assert!(!marker.exists());
    let first = execute(request.clone()).unwrap();
    assert_eq!(first.exit_code, 31);
    assert!(!first.result.retryable);
    assert_eq!(
        first.result.failure.unwrap().category,
        "ambiguous_execution"
    );
    assert_eq!(fs::read_to_string(&marker).unwrap(), "effect\n");
    let first_journal =
        fs::read_to_string(root.join("state/same-run/replay-events.jsonl")).unwrap();
    assert!(first_journal.contains("continuation_started"));
    assert!(!first_journal.contains("continuation_finished"));
    assert!(!first_journal.contains("tool_started"));
    request.output_dir = root.join("output-2");
    let rejected_output = request.output_dir.join("same-run");
    let second = execute(request).unwrap_err();
    assert_eq!(
        second.kind,
        pvisor_replay::ReplayErrorKind::AmbiguousExecution
    );
    assert_eq!(fs::read_to_string(&marker).unwrap(), "effect\n");
    assert!(!rejected_output.exists());
}
