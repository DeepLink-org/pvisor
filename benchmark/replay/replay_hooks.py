"""B-REPLAY: untimed reset and fidelity audit for each direct CLI execution."""

import argparse
import json
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from common import digest, save
from replay_fixtures import validate_prefix

SENTINEL = "existing workspace must survive preparation\n"


def prepare(batch, agent):
    fixture = batch / "fixtures" / agent
    protocol = json.loads((batch / "protocol.json").read_text())[agent]
    if digest(fixture / "trajectory.json") != protocol["sha256"]:
        raise ValueError("trajectory changed")
    for name in ("state", "output"):
        if (fixture / name).exists():
            raise ValueError("previous trial was not audited")
    if sorted(p.name for p in (fixture / "workspace").iterdir()) != ["existing"]:
        raise ValueError("prepare-only changed workspace inventory")
    if (fixture / "workspace/existing").read_text() != SENTINEL:
        raise ValueError("prepare-only changed workspace contents")


def verify(root, protocol, agent):
    results = list((root / "output").glob("*/result.json"))
    if len(results) != 1:
        raise ValueError("expected one replay result")
    result = json.loads(results[0].read_text())
    manifest = json.loads(results[0].with_name("manifest.json").read_text())
    if (
        result["failure"] is not None
        or result["replayed_tool_calls"] != 0
        or result["agent_status"] != "not_started"
        or result["phase"] != "prepared"
        or result["quality"] != "verified"
        or result["continued_steps"] != 0
        or result["after_step"] != 1
    ):
        raise ValueError("prepare-only executed tools, started an agent or failed")
    validate_prefix(
        manifest,
        results[0].parent,
        agent,
        protocol["command"],
        protocol["next_command"],
        protocol["sha256"],
    )
    return manifest["agent"]["profile"]


def conclude(batch, agent):
    prepare_workspace = batch / "fixtures" / agent
    protocol = json.loads((batch / "protocol.json").read_text())[agent]
    profile = verify(prepare_workspace, protocol, agent)
    # Both the result and the real host workspace must pass before archival.
    workspace = prepare_workspace / "workspace"
    if (
        sorted(p.name for p in workspace.iterdir()) != ["existing"]
        or (workspace / "existing").read_text() != SENTINEL
    ):
        raise ValueError("prepare-only modified the workspace")
    if digest(prepare_workspace / "trajectory.json") != protocol["sha256"]:
        raise ValueError("trajectory changed during execution")
    parent = batch / "trials" / agent
    parent.mkdir(parents=True, exist_ok=True)
    trial = parent / f"{len(list(parent.iterdir())) + 1:04d}"
    trial.mkdir()
    for name in ("output", "state"):
        shutil.move(str(prepare_workspace / name), str(trial / name))
    save(trial / "audit.json", {"profile": profile, "correctness": "passed", "executed_tools": 0})


def audit_replay(batch, manifest, results):
    protocol = json.loads((batch / "protocol.json").read_text())
    if digest(batch / "protocol.json") != manifest["protocol_sha256"]:
        raise ValueError("replay protocol changed")
    if set(protocol) != set(results):
        raise ValueError("incomplete adapter set")
    for agent in results:
        prepare(batch, agent)
        trials = sorted((batch / "trials" / agent).iterdir())
        if len(trials) != manifest["runs"] + manifest["warmup"] + 1:
            raise ValueError("missing replay preflight/warmup/sample fidelity audits")
        for trial in trials:
            profile = verify(trial, protocol[agent], agent)
            if json.loads((trial / "audit.json").read_text()) != {
                "profile": profile,
                "correctness": "passed",
                "executed_tools": 0,
            }:
                raise ValueError("invalid archived audit")


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("action", choices=("prepare", "conclude"))
    p.add_argument("batch", type=Path)
    p.add_argument(
        "agent",
        choices=(
            "claude-code",
            "codex",
            "opencode",
            "mini-swe-agent",
            "openhands",
            "pi-agent",
            "swe-agent",
        ),
    )
    args = p.parse_args()
    globals()[args.action](args.batch.resolve(), args.agent)
