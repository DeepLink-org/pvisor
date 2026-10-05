# Cluster quickstart: first task and two Workers

Start a Controller and two Workers on Linux, submit shell tasks, download evidence, then run dependency graphs and cancellation. `scripts/cluster-quickstart.py` verifies these steps using real binaries with explicit service and task resource limits.

| Path | Purpose |
| --- | --- |
| This guide | Build, prepare, start, tasks, evidence, DAGs, cancellation and cleanup |
| [Recovery and storage operations](operations.md) | Drain, restart reconciliation, loss, quotas, evidence retirement and GC |
| [VMs and model Gateway](vm-and-gateway.md) | Native VM controls/forks, offline models and provider configuration |
| [Complete design](../../design/cluster/index.md) | Authority, protocols, implementation and evolution constraints |

For one deployment entry managing Controller, Workers and node resources, see the [unified service guide](service.md). This page retains the independent-service task, reconciliation and storage acceptance workflow.

## Prerequisites and resource budget {#prerequisites}

Run from the repository root. You need Linux, Bash, Python 3.11+, Rust/Cargo, just, `/bin/sh`, `/bin/sleep`, cgroup v2 and a running user systemd manager. Prepare build dependencies using [Installation](../../start/installation.md). VMs also need x86-64, KVM/FUSE, `ldd` and an explicit firmware directory; see [VM preparation](vm-and-gateway.md#vm).

```bash
python3 --version
cargo --version
just --version
systemctl --user is-system-running
test -f /sys/fs/cgroup/cgroup.controllers
```

The user manager should report `running`. The bounded script requires at least 2 GiB `MemAvailable`, refusing startup otherwise; it never removes cgroup limits to bypass environment problems. This verified path is a Linux user-service deployment. macOS and containers without a user manager need separately configured and validated resource controls.

| Object | Concurrency / memory | CPU and other bounds |
| --- | --- | --- |
| Build | One Cargo job; 3 GiB for the entire build scope | At most one core; swap disabled |
| Controller | 256 MiB cgroup limit | At most 0.25 core |
| Each Worker and all descendants | One execution slot; 512 MiB cgroup limit | At most 0.5 core; swap disabled; at most 128 processes/threads |
| Each host task | 64 MiB address-space limit; 4 KiB retained output | Two seconds CPU time per process; 60-second wall timeout |
| Each VM | 128 MiB guest RAM, one vCPU | The entire one-slot Worker remains capped at 0.5 core; two seconds guest-command CPU time |
| Optional offline model service | 128 MiB cgroup limit | At most 0.1 core |

A session permits at most two concurrent sandboxes; ordinary examples wait between steps and use less. Do not run multiple sessions simultaneously. Two Workers plus the Controller have a combined 1.25 GiB hard memory limit; the Gateway example adds 128 MiB. Launcher, compiler and system costs outside the session are separate. `resources` are scheduling reservations; actual limits come from `run.runtime.resource_limits` and cgroups. Host is a trusted process backend; use the subsequent VM path for isolated execution.

## Build and verify automatically {#verify}

Build with the optional Gateway feature so subsequent model examples use the same binaries:

```bash
systemd-run --user --scope --quiet \
  --property=MemoryMax=3G --property=MemorySwapMax=0 --property=CPUQuota=100% \
  env CARGO_BUILD_JOBS=1 just --tempdir /tmp cluster-build-gateway
```

Verify the full workflow before manual operation. State must be a new directory outside the checkout; do not reuse previous task-ID history.

```bash
QS_ROOT=$(mktemp -d /tmp/pvisor-quickstart.XXXXXX)
python3 scripts/cluster-quickstart.py verify --backend host --state "$QS_ROOT/verify-host"
cat "$QS_ROOT/verify-host/report.json"
```

Expect `PASS` for every check and `passed: true` in `report.json`. The script checks actual kernel `memory.max` / `cpu.max` / `memory.swap.max`, OOM counters, outputs, placement, idempotency, downloads, DAGs, cancellation, drain, same-key restart recovery and storage retirement/GC. It attempts to stop its own services on success or failure while preserving state for diagnosis.

Verification executes workloads rather than merely parsing JSON. Each session gets random tokens, ports and systemd unit names and private copies of binaries, preventing concurrent Cargo builds from replacing VM reentry executables. Port races fail explicitly; retry with a new directory.

### Retained verification evidence {#evidence-results}

On 2026-10-05, these workflows ran sequentially on one Linux x86-64 host, stopping each session before starting the next. Kernel OOM/OOM-kill counters stayed zero. Peaks cover the entire Worker cgroup, including native descendants and file cache charged to it; they are neither guest RAM sizes nor general performance benchmarks.

| Path | Actual checks | Highest Worker cgroup memory peak | Record |
| --- | --- | --- | --- |
| Host | Tasks, placement, DAGs, cancellation, drain, restart and GC | About 13 MiB | [Host report](../../../assets/cluster-quickstart/host-20261005.json) |
| VM | Same plus pause/offload/resume/suspend and sealed-fork restore | About 272 MiB | [VM report](../../../assets/cluster-quickstart/vm-20261005.json) |
| Gateway | Basic workflow plus real HTTP, model authorization and credential isolation | About 23 MiB | [Gateway report](../../../assets/cluster-quickstart/gateway-20261005.json) |

Manual code blocks were also extracted from Markdown and executed in order, including resource queries, host restart-marker checks, retirement/GC, VM suspended-state waits and forks, and Gateway submit/download/stop. The [manual verification record](../../../assets/cluster-quickstart/walkthrough-20261005.json) retains block numbers and hashes. No run used more than two concurrent sandboxes. These records do not establish multi-host production capacity or external-provider acceptance.

## Prepare and start a manual session {#start}

```bash
QS_STATE="$QS_ROOT/manual-host"
python3 scripts/cluster-quickstart.py prepare --backend host --state "$QS_STATE"
source "$QS_STATE/env.sh"
trap 'python3 scripts/cluster-quickstart.py stop --state "$QS_STATE"' EXIT
python3 scripts/cluster-quickstart.py start --state "$QS_STATE"
python3 scripts/cluster-quickstart.py limits --state "$QS_STATE"
"$QS_BIN/pvisor-cluster" workers
```

`prepare` creates private configuration, task/DAG examples, an independent workspace, quotas and fixed copies of both binaries in `bin/`. `env.sh` and `session.json` have mode `0600`; source only your own generated file. `start` launches three user services with the limits above. `limits` verifies actual cgroup files and reports peaks, CPU use and OOM counters.

Expect `qs-a` and `qs-b`, each with `capacity.slots = 1`, `memory_bytes = 134217728` and `cpu_millis = 500`. Labels are `quickstart-node=a/b`. The Controller uses a 30-second lease, 64 MiB metadata quota and 128 MiB artifact quota to keep examples bounded.

## Submit and interpret the first task {#task}

```bash
cat "$QS_STATE/inputs/hello.json"
"$QS_BIN/pvisor-cluster" submit "$QS_STATE/inputs/hello.json"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task hello
"$QS_BIN/pvisor-cluster" show hello
"$QS_BIN/pvisor-cluster" submit "$QS_STATE/inputs/hello.json"
```

Expect terminal `phase = succeeded`, `result.exit_code = 0`, `result.output.stdout = "hello from Cluster\n"` and `lease.key.worker_id = qs-a`. Repeating the submission returns the original task without executing again; changing its command under the same ID conflicts. For different work, change `id` and `run.run_id` and preserve the old task.

| Field | Meaning |
| --- | --- |
| `execution` | Must match the Worker backend; host uses process/host_process |
| `resources` | Scheduling slot/RAM/CPU reservations, rather than kernel limits |
| `run.runtime.resource_limits` | Native memory/CPU-time/file limits; inspect Bundle observations for actual enforcement |
| `labels` | Node requirements; this task selects a |
| `retain_artifacts` | Require trace and Bundle delivery; VM examples also require a private upper |
| `reconciliation_pending` | Restart ownership remains unconfirmed; phase is historical |

`wait` defaults to aggregate terminal state with a 60-second deadline. When waiting for a particular state such as `running`, early terminal failure is reported immediately. Successful submission establishes accepted intent rather than native success or completed upload.

## Check the other node and download evidence {#evidence}

```bash
"$QS_BIN/pvisor-cluster" submit "$QS_STATE/inputs/other-node.json"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task other-node
"$QS_BIN/pvisor-cluster" artifacts hello
"$QS_BIN/pvisor-cluster" artifacts hello --out "$QS_STATE/downloads/manual-hello"
python3 - "$QS_STATE/downloads/manual-hello/run-bundle.json" <<'PY'
import json, pathlib, sys
bundle = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert bundle["run"]["run_id"] == "hello"
print("verified Run ID:", bundle["run"]["run_id"])
PY
```

The second task should succeed on `qs-b` with `node-b\n` stdout. Downloads contain `run-bundle.json` and `trace`; the client validates references/content and maintains a protection lease throughout transfer. Existing destination files are never overwritten; use a new directory for another download. Worker-local Bundle paths are not downloaded file paths.

## DAGs and cancellation {#dag}

```bash
"$QS_BIN/pvisor-cluster" graph submit "$QS_STATE/inputs/graph.json"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task dag-3
"$QS_BIN/pvisor-cluster" graph show quickstart-dag
"$QS_BIN/pvisor-cluster" submit "$QS_STATE/inputs/cancel-me.json"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task cancel-me --phase running
"$QS_BIN/pvisor-cluster" cancel cancel-me
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task cancel-me
```

The three graph nodes advance after predecessor aggregate success, ending with a `succeeded` graph; files are not automatically transferred. Cancellation first enters `cancelling`, then becomes `cancelled` after native termination and delivery settle. An accepted cancel is not observed termination.

## Stop, retain and proceed {#cleanup}

```bash
python3 scripts/cluster-quickstart.py limits --state "$QS_STATE"
python3 scripts/cluster-quickstart.py stop --state "$QS_STATE"
trap - EXIT
```

`stop` targets only this session's random units, preserving state and leaving other project services alone. The directory contains credentials, results and possible snapshots; do not commit or share it wholesale. Share only credential-free reports and selected artifacts. Delete your temporary directory only after services stop and retained evidence is no longer needed.

Proceed to [recovery and storage](operations.md), or use new directories for [VMs and Gateway](vm-and-gateway.md). Pin versions, inputs and compatibility before extending to multiple hosts. These two Workers are independent services on one host.
