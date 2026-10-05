# Cluster recovery and storage operations

Use the manual session started in the [first-task guide](index.md#start), retaining its `QS_STATE`, `QS_BIN` and environment. Download evidence before retirement makes remote evidence unavailable.

## Drain and resume admission {#drain}

```bash
"$QS_BIN/pvisor-cluster" drain qs-a
"$QS_BIN/pvisor-cluster" submit "$QS_STATE/inputs/drained.json"
"$QS_BIN/pvisor-cluster" show drained
"$QS_BIN/pvisor-cluster" drain qs-a --resume
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task drained
```

The drained task requires node a, so it remains `queued` during drain and reaches `succeeded` after admission resumes. Drain prevents new reservations while existing execution/delivery continues; it does not terminate processes or decommission a node.

## Restart the Controller without reexecution {#restart}

```bash
"$QS_BIN/pvisor-cluster" submit "$QS_STATE/inputs/restart-me.json"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task restart-me --phase running
"$QS_BIN/pvisor-cluster" show restart-me > "$QS_STATE/before-restart.json"
python3 scripts/cluster-quickstart.py restart-controller --state "$QS_STATE"
touch "$QS_STATE/workspace/release"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task restart-me > "$QS_STATE/after-restart.json"
python3 - "$QS_STATE" <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
before = json.loads((root / "before-restart.json").read_text())
after = json.loads((root / "after-restart.json").read_text())
assert before["lease"]["key"] == after["lease"]["key"]
assert after["phase"] == "succeeded"
assert (root / "workspace/once.txt").read_text() == "once\n"
print("same lease; one native execution; succeeded")
PY
```

This host example writes a once-only marker before waiting for the release file. Restart retains the journal/store while the Worker continues running. Original Worker polls clear pending and reconstruct deadlines. The same complete key and single marker establish no reexecution. Manual queries may miss the short pending window; automatic verification briefly stops the Worker main loop to inspect it deterministically.

Do not stop the Worker and adopt unknown execution using a new incarnation, or delete the log to reset it. Beyond the local watchdog deadline, inspect native termination and outbox delivery; late renewal cannot revive the old run.

## Permanent Worker loss {#lost}

Explicit loss resolution applies only to tasks with `reconciliation_pending` when the original Worker cannot recover. It is not an ordinary quickstart step and never triggers automatic retries.

Extract the complete `lease.key` from `show`, including task, Worker, incarnation and generation. Inspect native processes and external effects, then close the identity with `pvisor-cluster resolve-lost key.json`. Unknown outcomes become `lost`; known native outcomes are preserved. Replacement work uses new Task/Run/Attempt IDs. See [loss resolution](../../design/cluster/state-and-recovery.md#resolve-lost) for the contract.

## Storage policy, retirement and GC {#storage}

```bash
"$QS_BIN/pvisor-cluster" artifact-storage
"$QS_BIN/pvisor-cluster" artifact-storage --limits "$QS_STATE/artifact-limits.json"
RETIRE_BEFORE=$(python3 -c 'import time; print(int(time.time()*1000)+1)')
"$QS_BIN/pvisor-cluster" artifact-gc --retire-before-ms "$RETIRE_BEFORE" > "$QS_STATE/gc-plan.json"
python3 - "$QS_STATE/gc-plan.json" <<'PY'
import json, pathlib, sys
plan = json.loads(pathlib.Path(sys.argv[1]).read_text())
print("plan:", plan["id"])
print("retire:", [entry["task_id"] for entry in plan["retire"]])
print("objects:", len(plan["objects"]), "bytes:", plan["bytes"])
PY
PLAN_ID=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["id"])' "$QS_STATE/gc-plan.json")
"$QS_BIN/pvisor-cluster" artifact-gc --apply "$PLAN_ID"
"$QS_BIN/pvisor-cluster" show hello
```

Inspect the plan before apply: this cutoff retires previously completed terminal evidence, including hello. Its metadata/native result remains available afterward, while remote artifacts return 410. Existing downloaded files remain intact. Plans expire after five minutes and must be regenerated after restart. Active/shared references and download protection still prevent deletion. Any pending lease blocks destructive GC.

This does not clean Worker tasks, spools, image caches or snapshots, or shrink the metadata log. If metadata quota refuses first intents/results, inspect physical disk and increase log capacity; artifact GC cannot solve that limit. Automatic first-task verification includes actual online-policy, retirement and GC checks.

## Troubleshooting {#troubleshooting}

| Symptom | Check |
| --- | --- |
| Cannot connect to user systemd | Manager, session bus and cgroup delegation; the script never falls back to unlimited execution |
| Task remains queued | Backend, labels, model/environment capabilities, resources, tenant quotas and drain |
| VM spawn failure | Explicit firmware, KVM/FUSE access, ELF programs and libraries in rootfs |
| Native success but failed task | `artifact_error`, required upper/trace, upload quota and outbox |
| Persistent pending after restart | Original Worker, complete active inventory and incarnation; never automatically reassign |
| Existing download destination | Choose a new directory; overwrite is refused |
| 503 / 507 | Overload/uncertain I/O versus definite quota refusal; retry the same identity |

Private `session.json` records the service-name `prefix`. Use `journalctl --user -u <prefix>-worker-a.service` for its logs, or inspect native records under `worker-a/tasks/`. Do not publish the session's tokens. Finish with [cleanup](index.md#cleanup).
