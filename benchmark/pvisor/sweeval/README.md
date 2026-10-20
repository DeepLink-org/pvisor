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

Set an explicit OpenAI-compatible API base and the provider's model ID. There
is no default provider, model, or provider-specific reasoning/sampling policy:

```bash
source benchmark/pvisor/sweeval/model.env.example
# Replace OPENAI_BASE_URL and OPENAI_MODEL placeholders before running.
read -rs -p 'Model API key: ' OPENAI_API_KEY; echo
export OPENAI_API_KEY
```

The API base must not end in `/chat/completions`; the client adds it once.
Credentials stay in the host environment and are not serialized into model
configs or guest environments. Only `OPENAI_API_KEY` is read; legacy
`GLM_*` and `SWE_MODEL_PROFILE` settings are no longer used.

`SWE_MODEL_KWARGS` is an optional JSON object for non-secret request options:
`temperature`, `top_p`, `max_tokens`, `timeout`, `reasoning_effort`, and
`extra_body` for provider-specific fields. For example:

```bash
export SWE_MODEL_KWARGS='{"max_tokens":8192,"temperature":0.5}'
```

Do not put credentials into these options: they are retained as experiment
metadata. Endpoint, model, messages, tools, and credentials cannot be overridden
through request options. Defaults are 16384 output tokens, a 120-second request
timeout and zero client retries; upstream Agent retries are bounded by
`MSWEA_MODEL_RETRY_STOP_AFTER_ATTEMPT=2`. Reasoning extras are sent only when
explicitly configured. The upstream mini `litellm` client adds its internal
`openai/` prefix; the service receives the model ID without that prefix.

Run the explicit API-only preflight after setting the key. It makes two
potentially billable requests, validates a native tool call using a fixed tool
result, and never executes the generated command:

```bash
"$SWE_ROOT/venv/bin/python" benchmark/pvisor/sweeval/preflight.py \
  --output "$SWE_ROOT/api-preflight/model-NEW"
```

Inspect retained responses and failures before starting tasks. This checks the
configured endpoint's tool-call exchange, not SWE solving ability or backend
model identity. Preflight and real runs use the same request options; model
settings are retained in each run's `model-settings.json`.

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

## Recorded trajectory data

The 30 recorded tool trajectories and detailed historical validation records
are a separate data package, not embedded benchmark source. See
[the data-package manifest and usage notes](traces/README.md) for its checksum,
contents, publication status and validation limits. Full private logs remain
in the experiment archive. This adapter does not add a second replay engine.
