# SWE resident integration (B-SWE-RESIDENT-ENG)

This engineering entry runs the existing mini-swe-agent 2.4.6 on the host and
executes its tools in one persistent native pVisor VM per task. It reuses
SweEval's `SingleStepTrial`, custom environment factory, initialization, patch
collection, verifier and native-to-ATIF conversion. It does not add a new model
Agent loop or use Docker to execute tools. Model credentials stay on the host; mini-swe-agent configuration is isolated
inside each run directory.

The first cohort is SWE-bench Pro NodeBB 291, Vuls 666 and qutebrowser 667.
The task package does not contain completed trajectories: the first model run
creates them. This adapter accepts shared verifiers only; DeepSWE separate
verifier images and arbitrary host mounts are outside this first cohort.

## Prerequisites and preparation

Linux x86_64 with KVM, a delegated cgroup v2 systemd user manager, `skopeo`,
`umoci`, and a compatible pVisor firmware directory are required. This adapter
uses the externally supplied SweEval 0.1.0 package (supplied archive `SweEval-1e31b894.tar.gz`); it is not
vendored or fetched by these commands. The dataset and task licenses remain
separate. mini-swe-agent is pinned because the timing adapter wraps its model
query method. Install in an isolated Python 3.11+ environment; record the full
resolved dependency list with each experiment.

```bash
export SWE_ROOT=/data/your-user/swe-resident
export PVISOR_FIRMWARE=/absolute/path/to/firmware
export UMOCI=/absolute/path/to/umoci
# Place the supplied SweEval package at "$SWE_ROOT/SweEval" first.
python3 -m venv "$SWE_ROOT/venv"
"$SWE_ROOT/venv/bin/python" -m pip install -e "$SWE_ROOT/SweEval" \
  'mini-swe-agent==2.4.6' 'litellm==1.75.5.post2' 'openai==2.54.0' \
  'pytest==8.4.2' 'ruff==0.17.0'
# Run from the pVisor checkout. Build before, never during, measurement.
export CARGO_TARGET_DIR="$SWE_ROOT/build"
cargo build --release -p pvisor --example sweeval_resident
cp benchmark/pvisor/sweeval/resident.example.json "$SWE_ROOT/resident.json"
"$SWE_ROOT/venv/bin/python" benchmark/pvisor/sweeval/run.py \
  --config "$SWE_ROOT/resident.json" --plan
"$SWE_ROOT/venv/bin/python" benchmark/pvisor/sweeval/prepare.py \
  --config "$SWE_ROOT/resident.json"
```

The example selects NodeBB 291, Vuls 666 and qutebrowser 667. Edit `tasks` to
select another explicit cohort. Path fields support `${VARIABLE}` substitution;
missing variables fail before preparation. AppArmor and host proxy settings are
optional, explicit host choices; no host-specific policy or route is installed.
Build and experiment outputs use `/data`, and each run needs a new output path.

By default preparation uses the task's image. For private registries unavailable
on the host, set each task entry's `image` to its corresponding public image from
the task Dockerfile (for these tasks, `docker.io/jefzda/sweap-images`). Do not
substitute an arbitrary SWE image. `resident.public.example.json` supplies the
retained public digests and base revisions for the three example tasks; copy it
instead of `resident.example.json` to use that route. `prepare.py` resolves a linux/amd64 digest,
copies it with skopeo, unpacks with umoci, and restores OCI ownership/mode metadata
for the virtual filesystem. It records both the task's source image and the
selected image without editing the dataset. Every failed attempt remains on disk;
existing prepared descriptors are never overwritten. Repeated layers are handled
by skopeo; the host-specific manual blob-linking batch script is not included.

Public base images may omit task Dockerfile setup. Supply the exact `base_commit`,
`workdir`, `python` and `environment` overrides in the entry when required.
For the tested public cohort the workdir is `/app`, Python is `/usr/bin/python3`,
and `PYTHONPATH=/app/lib:/app`. The optional uv bootstrap was omitted because
these three verifiers do not use it and the Agent runs on the host. Other tasks
may need additional setup; a passing reference verifier is required before model
execution. This is not a general Dockerfile interpreter or proof that public
images have the same bytes as private task images.

Preparation only unpacks rootfs data on the host. When `base_commit` is set,
the VM initializes its private overlay and verifies the Git revision before
running SweEval. `/etc/hosts` gets standard localhost entries if absent;
`TMPDIR=/tmp` uses the private overlay instead of the runtime's scratch tmpfs.
Keep immutable prepared rootfs inputs and record the source, binary, firmware,
task package, configuration and dependency hashes alongside each batch.

```bash
"$SWE_ROOT/venv/bin/python" benchmark/pvisor/sweeval/run.py --oracle \
  --config "$SWE_ROOT/resident.json" --output "$SWE_ROOT/runs/oracle-NEW"
```

The original reference solution and verifier run without an API key, in separate
fresh VM overlays. `require_oracle` must name that run's `oracle-gate.json` before
a model run; every selected task must have reward 1 and the same image digest
and `krun_enomem_workaround` setting. This gate is an operational precondition,
not a complete content attestation: revalidate after changing task files,
rootfs contents, firmware, binary or any execution settings. Keep initialization
and oracle timing separate from Agent measurements.

## Configure the model on the host

The default profile is Qwen through the lab's OpenAI-compatible endpoint.
Use the service alias `agents-a1` in requests. The user identifies its deployment
as Qwen3.6-35B-A3B-FP8; the alias alone does not verify the backend weights/version.
In the host Bash shell:

```bash
source benchmark/pvisor/sweeval/qwen.env.example
read -rs -p 'Lab API key: ' OPENAI_API_KEY; echo
export OPENAI_API_KEY
```

The template sets `SWE_MODEL_PROFILE=qwen`,
`OPENAI_BASE_URL=https://token.pjlab.org.cn/v1`, `OPENAI_MODEL=agents-a1`.
Do not append `/chat/completions`: the client adds it once. Keys remain on the
host and are not serialized into model configs or guest environments.
Qwen requests omit GLM-specific `thinking`, `clear_thinking`, `reasoning_effort`,
and sampling overrides. Thinking behavior follows the service defaults;
returned `reasoning_content` is retained by the existing mini client.
The adapter requests at most 16384 output tokens and a 120-second request timeout;
these limits and native tool calling still need a real service preflight.
A text-only hello response does not establish tool-call compatibility.

Run the small API-only preflight after setting the key (two potentially billable
requests, no VM or dataset needed):

```bash
"$SWE_ROOT/venv/bin/python" benchmark/pvisor/sweeval/preflight.py \
  --output "$SWE_ROOT/api-preflight/qwen-NEW"
```

It requests a harmless tool call, validates its name/arguments, returns a fixed
tool result without executing the command, then checks the model's final reply.
Both responses, usage and failure status are retained without request credentials.
If it fails, inspect the retained response before launching the three tasks.
It does not prove SWE solving ability or the identity of the deployed weights.

The optional `SWE_MODEL_PROFILE=glm` enables the previous GLM-specific extras.
For that profile only, legacy `GLM_BASE_URL`, `GLM_MODEL`, `GLM_API_KEY` are
fallbacks when the corresponding `OPENAI_*` setting is absent. Qwen never uses
a leftover GLM key. Unset old `OPENAI_*` values when switching endpoints.
Both profiles use the upstream mini `litellm` class with the internal `openai/`
prefix; the service receives the plain model ID. The model selection is retained
in each run's `model-settings.json`. `MSWEA_MODEL_RETRY_STOP_AFTER_ATTEMPT=2`
bounds upstream mini retries. No external service compatibility is claimed until
a credentialed preflight passes.

```bash
umask 077
"$SWE_ROOT/venv/bin/python" benchmark/pvisor/sweeval/run.py \
  --config "$SWE_ROOT/resident.json" \
  --output "$SWE_ROOT/runs/resident-NEW"
```

The output directory must be new and under `/data`. Selected tasks run serially,
with a 200-step budget per task and SweEval task-defined timeouts. This is an
explicit, potentially billable model run; `--plan` never calls the model.
Missing prepared images or credentials fail before launching any task.

## Resource records and limits

The example tasks use 2 vCPUs and 7168 MiB guest RAM. Task-defined resources
determine the service CPU quota and memory cap (guest RAM plus 2048 MiB), with
zero swap. For the example this is two cores and 9216 MiB, including
VMM/helper overhead. The controller/Agent and external model are outside it.
The VM service has an explicit 65536-file descriptor limit: the host virtio-fs
backend can exceed systemd's 1024 soft default during npm installation. This
limit applies only to the experiment service, not the host's global policy.
`apparmor_profile`, when set, must name an existing profile allowing the runtime's
user namespaces; the adapter does not install or modify system policy. All native backing data
are retained under the task output, including RAM and failures. The service uses
`KillMode=mixed`: SIGTERM reaches the driver first, which cancels through pVisor
and awaits its result; the service timeout still terminates remaining processes.
`run-result.json` records completion or cancellation, and `driver-shutdown.json`
records the signal. Cancellation during teardown is not reported as natural VM
completion. `smoke.py --force-stop` checks this path on KVM.

`vm/resources.jsonl` samples cgroup CPU, memory.current/peak/stat, swap and OOM
counters every 250 ms. `vm/events.jsonl` records host monotonic model request,
tool and trial phase boundaries. API wait includes queue, reasoning, generation
and network; it is not pure reasoning time. Background guest processes can
continue during model wait. No automatic pause/offload policy is enabled.

```bash
"$SWE_ROOT/venv/bin/python" benchmark/pvisor/sweeval/analyze.py \
  "$SWE_ROOT/runs/resident-NEW/nodebb-291/vm"
```

This writes `timeline.csv` and `phase-summary.json`. Phase labels use interval
midpoints (up to one sample interval of boundary uncertainty). Preserve native
Agent trajectories, task result.json, verifier output, VM RunResult and all
failures. Final success/reward comes from SweEval, never a model assertion.

The mailbox is a single-controller experiment protocol, not a multi-tenant
authenticated RPC service. Host opens are pinned to a directory FD and refuse
symlinks; guest outputs must not grant access to arbitrary host files.
Command timeouts/cancellation kill the command process group. Detached process
groups remain contained in the VM until teardown; do not equate tool completion
with absence of guest background work. File downloads use tar's data filter.
The controller also checks VM process liveness while awaiting a command result,
so a crashed worker fails promptly instead of waiting for the tool timeout.

## Validation and scope

`test_adapter.py` tests the guest protocol and native mini loop without a model
service. Tests needing the external SweEval/mini packages skip when those
optional dependencies are absent; they must run with the documented environment
before using this adapter. The repository's default `just test-py` uses unittest
and does not run this opt-in pytest integration suite. `smoke.py` validates real KVM command state, timeout and file transfers
using a prepared rootfs. `trial_smoke.py` uses a local HTTP model fixture plus
real KVM to validate SweEval through its verifier. These are integration checks,
not the three real SWE tasks or model performance measurements.

The runtime compatibility fixes are supplied separately by
[PR #28](https://github.com/DeepLink-org/pvisor/pull/28): DNS lower-layer
metadata alignment, handle-based truncation after unlink/rename, and guest
ownership of new directories under rootless virtual metadata. This benchmark
diff contains no runtime changes. Use a checkout containing those fixes.

The optional JSON setting `krun_enomem_workaround` defaults to `null`, preserving
the runtime's kernel-dependent default. An explicit `false` selects the existing
diagnostic override for a validated host; it removes a 5 ms sleep before each
KVM_RUN. Record it in `vm.json` and validate the same setting with the protocol
smoke and reference patches before a model run. This is not a change to pVisor's
default compatibility policy. The oracle gate must match this setting as well as
the pinned image digests. Retain interrupted/default-setting trials separately.

`host_http_proxy` can explicitly supply a credential-free HTTP proxy endpoint
to the systemd VM service. It is a host upstream route for the existing pVisor
network connector, not a guest environment variable. A shell's proxy exports
are not automatically inherited by a new systemd service. Verify package
downloads inside a VM before reference validation; retain direct-connect
failures separately. Any configured proxy must remain available throughout the run.

Native recorded trajectories retain upstream mini format/config for later
replay inside a fresh guest. The resource experiment distinguishes replaying
tools in a new VM from resuming the memory state of the same VM.

Offline adapter checks (no external API, image download or VM):

```bash
"$SWE_ROOT/venv/bin/ruff" check benchmark/pvisor/sweeval
"$SWE_ROOT/venv/bin/python" -m pytest -q benchmark/pvisor/sweeval/test_adapter.py
```

Full low-CPU-window/activity reports and host-specific batch/retry orchestration
remain separate experiment tooling. `analyze.py` here provides per-VM phase
summaries only. It does not identify download bytes, lazy-image fetches, or pure
reasoning time; command text alone is insufficient evidence for those claims.
Keep different cohorts, binaries and settings separate in any result table.

Shutdown allows up to 300 seconds for the driver to complete runtime teardown and retain the overlay/run bundle after the guest stop acknowledgment. This is teardown, excluded from Agent resource statistics. A stop acknowledgment alone is not a completed VM result; `vm_stop` records whether the driver retained its terminal result.
