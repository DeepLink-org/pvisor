//! Untimed per-operation preparation and native-prefix fidelity checks.

use super::fixtures::{COMMAND, NEXT_COMMAND, OBSERVATION, SENTINEL, nested_strings};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path};

pub(super) fn digest(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(path)?).with_context(|| format!("parse {}", path.display()))
}

pub(super) fn unchanged(root: &Path, sha: &str) -> Result<()> {
    ensure!(
        digest(&root.join("trajectory.json"))? == sha,
        "trajectory changed"
    );
    let workspace = root.join("workspace");
    let entries = fs::read_dir(&workspace)?.collect::<std::io::Result<Vec<_>>>()?;
    ensure!(
        entries.len() == 1 && entries[0].file_name() == "existing",
        "prepare-only changed workspace inventory"
    );
    let sentinel = workspace.join("existing");
    ensure!(
        fs::symlink_metadata(&workspace)?.is_dir() && fs::symlink_metadata(&sentinel)?.is_file(),
        "workspace or sentinel replaced with non-regular entry"
    );
    ensure!(
        fs::read_to_string(sentinel)? == SENTINEL,
        "prepare-only changed workspace contents"
    );
    Ok(())
}

pub(super) fn prepare(root: &Path, sha: &str) -> Result<()> {
    unchanged(root, sha)?;
    for name in ["state", "output"] {
        ensure!(
            !root.join(name).try_exists()?,
            "previous trial was not audited"
        );
    }
    Ok(())
}

pub(super) fn validate_prefix(
    manifest: &Value,
    native: &Value,
    agent: &str,
    sha: &str,
) -> Result<()> {
    ensure!(
        manifest["agent"]["name"] == agent
            && manifest["source"]["sha256"] == sha
            && manifest["boundary"]["after_step"] == 1
            && manifest["boundary"]["tool_calls"] == 1
            && manifest["boundary"]["complete_tool_batch"] == true,
        "wrong source, adapter or complete prefix boundary"
    );
    let batches = manifest["batches"].as_array().context("missing batches")?;
    ensure!(batches.len() == 1, "wrong prefix batch count");
    let calls = batches[0]["tool_calls"]
        .as_array()
        .context("missing tool calls")?;
    ensure!(calls.len() == 1, "wrong prefix tool count");
    let args = &calls[0]["arguments"];
    let command = args
        .get("command")
        .or_else(|| args.get("cmd"))
        .or_else(|| args.get("raw_action"));
    ensure!(
        command == Some(&Value::String(COMMAND.into())),
        "prefix tool arguments differ"
    );
    let mut strings = Vec::new();
    nested_strings(native, &mut strings);
    ensure!(
        strings.iter().any(|s| s == COMMAND)
            && strings.iter().any(|s| s == OBSERVATION)
            && !strings.iter().any(|s| s == NEXT_COMMAND),
        "native prefix lost tool/observation or included action beyond boundary"
    );
    Ok(())
}

pub(super) fn verify(root: &Path, agent: &str, sha: &str) -> Result<Value> {
    let mut results = Vec::new();
    for entry in fs::read_dir(root.join("output"))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().join("result.json").is_file() {
            results.push(entry.path());
        }
    }
    ensure!(results.len() == 1, "expected one replay result");
    let output = &results[0];
    let result = read_json(&output.join("result.json"))?;
    // Missing failure is not equivalent to an explicit null success result.
    ensure!(
        result.get("failure") == Some(&Value::Null)
            && result["replayed_tool_calls"] == 0
            && result["agent_status"] == "not_started"
            && result["phase"] == "prepared"
            && result["quality"] == "verified"
            && result["continued_steps"] == 0
            && result["after_step"] == 1,
        "prepare-only executed tools, started an agent or failed"
    );
    let manifest = read_json(&output.join("manifest.json"))?;
    let filename = match agent {
        "mini-swe-agent" => "prepared-prefix.json",
        "openhands" => "prepared-replay-events.json",
        "swe-agent" => "prepared-prefix.traj",
        _ => "prepared-prefix.jsonl",
    };
    let path = output.join("native").join(filename);
    let native = if filename.ends_with(".jsonl") {
        let text = fs::read_to_string(path)?;
        Value::Array(
            text.lines()
                .filter(|line| !line.trim().is_empty())
                .map(serde_json::from_str)
                .collect::<serde_json::Result<Vec<_>>>()?,
        )
    } else {
        read_json(&path)?
    };
    validate_prefix(&manifest, &native, agent, sha)?;
    unchanged(root, sha)?;
    let Some(profile) = manifest["agent"].get("profile") else {
        bail!("missing agent profile")
    };
    Ok(profile.clone())
}
