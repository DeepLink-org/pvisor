# Cluster VMs and model Gateway

Use new session directories for isolation and model integration. Offline VM snapshot controls and network Gateway use separate profiles. Stop the previous session first and retain at most two concurrent executions.

## Native VMs: preparation and full verification {#vm}

The verified target is Linux x86-64 with a glibc source build, readable/writable `/dev/kvm` and `/dev/fuse`, `ldd`, and an explicit directory containing `libkrunfw.so.5`. Static musl embedded-kernel execution and macOS HVF are outside this script's validation scope.

```bash
test -r /dev/kvm && test -w /dev/kvm
test -r /dev/fuse && test -w /dev/fuse
QS_FIRMWARE=$(python3 - <<'PY'
import sys
sys.path.insert(0, "scripts/packaging")
from stage_wheel_binaries import BuildOptions, _firmware_source
print(_firmware_source(BuildOptions())[0].parent)
PY
)
test -f "$QS_FIRMWARE/libkrunfw.so.5"
python3 scripts/cluster-quickstart.py verify --backend vm \
  --firmware-dir "$QS_FIRMWARE" --state "$QS_ROOT/verify-vm"
cat "$QS_ROOT/verify-vm/report.json"
```

Firmware preparation reuses project packaging: prefer `PVISOR_LIBKRUNFW_PATH`, otherwise use cached firmware or download a pinned release, check archive SHA-256 and extract it. First preparation needs GitHub access. Provide correct firmware in advance offline; do not skip file checks. Offline checkpoint registration requires an explicit directory. Packaging resides in `scripts/packaging/stage_wheel_binaries.py`; the native loader is in `crates/pvisor-vm/src/firmware_store.rs`.

The script copies only host `/bin/sh`, `/bin/sleep` and their `ldd` dependencies into a small rootfs. It neither pulls large images nor uses the host `/` as a lower. The rootfs is also configured as the read-only lower, with private Attempt uppers for execution, artifact export and sealed checkpoints. Each VM has 128 MiB/one vCPU; each one-slot Worker's full process tree remains capped at 512 MiB/0.5 core.

Expect the basic workflow and `native VM pause/offload/resume/suspend and sealed-fork restore` to pass. Restore uses a real native snapshot, with a native pause ACK after restore confirming execution. No simulated VM results are used.

## Manual pause, offload, suspension and fork {#controls}

```bash
QS_STATE="$QS_ROOT/manual-vm"
python3 scripts/cluster-quickstart.py prepare --backend vm \
  --firmware-dir "$QS_FIRMWARE" --state "$QS_STATE"
source "$QS_STATE/env.sh"
trap 'python3 scripts/cluster-quickstart.py stop --state "$QS_STATE"' EXIT
python3 scripts/cluster-quickstart.py start --state "$QS_STATE"
"$QS_BIN/pvisor-cluster" submit "$QS_STATE/inputs/vm-controls.json"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task vm-controls --phase running
"$QS_BIN/pvisor-cluster" control vm-controls pause --request-id pause-1
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task vm-controls --phase paused
"$QS_BIN/pvisor-cluster" control vm-controls offload --request-id offload-1
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task vm-controls --phase offloaded
"$QS_BIN/pvisor-cluster" control vm-controls resume --request-id resume-1
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task vm-controls --phase running
"$QS_BIN/pvisor-cluster" control vm-controls suspend --request-id suspend-1
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task vm-controls --phase suspended
"$QS_BIN/pvisor-cluster" fork vm-controls "$QS_STATE/inputs/fork.json"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task vm-child --phase running
"$QS_BIN/pvisor-cluster" control vm-child pause --request-id child-pause
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task vm-child --phase paused
"$QS_BIN/pvisor-cluster" cancel vm-child
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task vm-child
python3 scripts/cluster-quickstart.py limits --state "$QS_STATE"
python3 scripts/cluster-quickstart.py stop --state "$QS_STATE"
trap - EXIT
```

Wait for each native observation before the next action. Pause/offload retain RAM and slots while releasing logical CPU; resume readmits; suspend seals full state and stops the source, releasing all reservations only at terminal acceptance. Fork creates one child after the source is suspended, so it does not add a concurrently running source VM.

The control example sleeps for 240 seconds with a 300-second wall timeout, leaving time for manual operation while retaining the same hard memory/CPU limits. Reading too long can still allow termination; retry in a new session rather than reusing terminal IDs. Repeated control/fork IDs are idempotent only for identical contents. Creation receipts prove creation, while post-restore native ACKs validate execution. Cross-host migration and multiple live-fork branches are outside this quickstart.

## Offline Gateway without a provider account {#gateway}

Use a Gateway-feature build to make real HTTP requests through an actual Attempt Gateway. The additional loopback model serves deterministic responses under a 128 MiB/0.1-core hard limit. It calls no external model and measures neither quality nor provider performance.

```bash
python3 scripts/cluster-quickstart.py verify --backend host --gateway \
  --state "$QS_ROOT/verify-gateway"
cat "$QS_ROOT/verify-gateway/report.json"
```

Expect `Attempt Gateway model request, forbidden model and credential isolation`: an allowed request receives its response; an unauthorized model gets 403 without reaching upstream. Agents receive a local placeholder credential and no Controller/Worker/provider tokens. Bundle and mixed native/model trace are actually delivered and downloaded.

Submit the same task manually:

```bash
QS_STATE="$QS_ROOT/manual-gateway"
python3 scripts/cluster-quickstart.py prepare --backend host --gateway --state "$QS_STATE"
source "$QS_STATE/env.sh"
trap 'python3 scripts/cluster-quickstart.py stop --state "$QS_STATE"' EXIT
python3 scripts/cluster-quickstart.py start --state "$QS_STATE"
"$QS_BIN/pvisor-cluster" submit "$QS_STATE/inputs/gateway-agent.json"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task gateway-agent
"$QS_BIN/pvisor-cluster" artifacts gateway-agent --out "$QS_STATE/downloads/manual-agent"
python3 scripts/cluster-quickstart.py stop --state "$QS_STATE"
trap - EXIT
```

Expected stdout is `Gateway request passed; unauthorized model denied; credentials isolated`. `gateway.routes` in `worker.toml` defines routing; task `gateway.models` requests node capabilities, while `run.capabilities.models` grants access. Model matching and capture levels must agree.

## Connect a real Agent and provider {#production}

Replace the Worker's `gateway.routes[].upstream` with the provider API base and set `api_key_env` to its credential in the Worker service environment. Inline `api_key` is prohibited. Update concrete model requirements, capabilities and Agent commands with new Task/Run IDs, checking network policy and backend isolation first. Agent commands and provider environment variables are covered in [Agent integration](../agents/index.md).

Real models need accounts, network access and provider contracts; the offline checks do not validate those. VM inference waits also require a network profile, `release_cpu_on_idle` and whole-guest idleness declarations. Do not enable them directly on the offline snapshot profile. See [inference-wait design](../../design/cluster/lifecycle.md#inference) for ordering.

Multiple hosts require pinned rootfs/environment inputs, reachable URLs, TLS, node storage and compatible checkpoints. Copying local directories does not establish these contracts. See [verification](index.md#verify) for this quickstart's evidence and resource observations.
