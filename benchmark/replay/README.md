# Replay benchmark tools

The existing B-REPLAY `replay.md` / `prepare.py` entry measures native trajectory
preparation by `pvisor replay`. It remains unchanged. It is **not** the fixed
command workload below: native replay rebuilds Agent context and can continue
inference; command replay never calls a model or reconstructs that context.

## Model-free command workloads (B-SWE-RESIDENT-ENG)

`workload.py` extracts the previously validated experiment's sequential command,
source-outcome and host-side wait semantics into one reusable runner. Both
transports use that runner, initialization, official classification and clock.
It accepts normalized E3-style rounds, not arbitrary native Agent trajectories.
Do not use it as a replacement for the native replay adapters.

No sandbox starts on import or on the default validation invocation:

```sh
python3 benchmark/replay/workload.py --plan plan.json --backend backend.json \
  --output benchmark/replay/.data/new-task
# Explicitly execute ONE task after provisioning and admission:
python3 benchmark/replay/workload.py --plan plan.json --backend backend.json \
  --output benchmark/replay/.data/new-task --execute
```

Outputs must be fresh. Commands are trusted workload code, executed as root in
the sandbox. The caller supplies the prepared image/template, matching initial
commit/dependencies, environment, outer CPU/RAM budget, admission and supervision.
Image/template preparation is outside the clock. Never put credentials in plans.
The runner does not download images, start services, change resource policies,
install global networking, or schedule a batch. No implicit 30-second command
timeout is applied. Use an enclosing finite supervisor; SDK RPC deadlines are
transport limits, not guest tool deadlines. A killed controller may not finish
cleanup; the enclosing supervisor must audit owned resources.

A minimal plan (replace the toy evaluator with the task's official test script
and parser; parser stdout must be a JSON mapping from exact test name to status):

```json
{
  "schema": "command-replay/v1",
  "python": "/usr/bin/python3",
  "initial_check": "test \"$(git rev-parse HEAD)\" = EXPECTED_COMMIT && test -z \"$(git status --porcelain)\"",
  "rounds": [
    {"round": 1, "wait_s": 1.5, "source_outcome": "completed",
     "command": "printf hello", "source_returncode": 0}
  ],
  "official": {
    "command": "./run-official-tests.sh",
    "read_results_command": "python3 parse-official-tests.py",
    "required_tests": ["test_example"]
  }
}
```

`wait_s` is elapsed host waiting after the preceding local tool, not the original
absolute wall-clock timestamp. Null means no known wait. `not_executed` skips the
command but preserves its wait; `timeout` reexecutes without a 30-second deadline
and keeps the source annotation. Nonzero exits and source exit mismatches are
observations, not silently reclassified as infrastructure failure. Outputs are
hashed per step; this is not a guarantee of semantic equivalence. The official
command must include any patch extraction/application and test preparation
required by the evaluator. No task-specific reset or patch substitution is
inferred by this runner.

Both backends fill `/etc/hosts` **only when empty**, then verify localhost lookup
and socket binding with the configured guest Python. An existing broken hosts
file fails preflight; it is not overwritten. This aligns initialization, rather
than optimizing AgentENV. `initial_check` must separately verify the environment.

### Backends

pVisor uses the current container CLI (`run`, `exec`, `rm -f`):

```json
{"kind":"pvisor","binary":"/absolute/path/to/pvisor","image":"prepared-image:tag",
 "vcpus":2,"memory":"8g","cwd":"/app"}
```

To select pVisor's native userspace UKSM-style physical page sharing, add
`"memory_pool":"/absolute/path/to/pool.sock"` to the pVisor backend configuration.
The adapter forwards it to the existing `run --memory-pool` option; it does not
change the sharing algorithm or enable host kernel KSM/UKSM. Omit the field to
retain the existing unshared behavior. The AgentENV backend rejects this field.

Provision a native physical-page pool externally before execution (not a
compressed pool); the socket alone does not select or verify the server mode. Only an absolute socket path is
accepted (`auto` is deliberately rejected to avoid implicit service startup).
The CLI checks connection availability at execution time; validation-only does
not contact the pool. The caller owns pool sizing, connection capacity, its CPU
and memory accounting within the parent budget, and its eventual shutdown after
all clients exit. Task cleanup removes only the task container, never the pool.
The runner does not configure offload, backing compression, or guest-idle handoff.

This is a current-main transport, **not the retired `sweeval_resident` offload
harness**. It does not enable or reproduce historical 50 ms management/backing
compression. Historical validation numbers must not be attributed to this CLI
transport. No product Rust replay, offload or compression code is changed here.

AgentENV is optional; only its transport imports the SDK. Install `e2b==2.5.0`
in a separate environment. The adapter retains the validated single-endpoint
AgentENV routing headers; the SDK's private creation interface is pinned here.

```json
{"kind":"agentenv","endpoint":"http://127.0.0.1:18250","template":"prepared-template",
 "api_key_env":"AGENTENV_API_KEY","vm_timeout_s":18000,"cwd":"/app"}
```

The existing AgentENV service and template determine VM CPU/RAM; this file does
not configure them. Verify those settings and the service/ublk/VM cgroup budget
before admission. API keys come from the named environment variable and are not
written to results. AgentENV is used natively: no pause, resume, snapshot, active
offload or compression calls are added. No AgentENV server source is changed.

### Results and timing

`total_s` starts immediately before creation and ends after deletion returns
(or fails), for **both** transports. Disjoint phases: `create_s`, `initialize_s`,
`replay_s`, `official_s`, `cleanup_s`; `model_wait_s` and `tool_s` are components
of replay. Tool time includes transport/recovery overhead, not just guest CPU.
Queue time is external and is not fabricated as zero. Small persistence costs
are included; phase sums need not equal total exactly.

`execution_completed` requires replay and API cleanup without execution errors;
it is separate from `official.passed`. Internal/collection errors, missing
required tests, parser errors and setup statuses classify as evaluator
infrastructure errors. Complete parsed assertion failures classify as
`tests_failed`, even when the evaluator wrapper exits zero. A shell return code
alone never proves official success. CLI exit 2 means execution/evaluator failure;
ordinary official test failures remain recorded data with CLI exit 0.

`api_deleted` is only transport acknowledgment. **Neither backend claims physical
exit verification**: retain host PID identity, cgroup and mount/workspace audits
from the deployment harness before counting a strict completion or admitting
another task after uncertain cleanup. Partial creation errors remain failures
requiring scoped inspection, never guessed cleanup success. No memory-savings
claim is produced: template caches and shared read-only root pages require a
separate, aligned accounting boundary.

### Validation

```sh
mise run test-py -- discover -s benchmark/tests -p test_workload.py
```

Tests use fake transports and clocks, with no VM, AgentENV service, model call or
benchmark run. Historical single-task localhost validation motivated these
contracts; it is not end-to-end validation of this refactoring or of current main.

### Concurrent controller sampling and wait diagnostics

`resource_sampler.py` and `wait_timing.py` are opt-in B-SWE-RESIDENT-ENG
helpers extracted from the 80-trajectory deployment controller. They do not
change the synchronous single-task runner or the product VM runtime. Both
pVisor and AgentENV controllers can use the same interfaces.

```python
from resource_sampler import ResourceSampler, collect_to_log

async with ResourceSampler(workers=4) as sampler:
    # Each observer awaits a read, then sleeps 0.5s; never queue catch-up reads.
    await sampler.read(collect_to_log, collect_vm, owned_record, group_name,
                       log_path, submitted_monotonic_ns, due_monotonic_ns)
    sampler.check()
```

The deployment must verify PID/starttime and cgroup ownership, create fresh
output files, supervise observers, and propagate observer failure. Collection,
JSON serialization and persistence belong in the worker, not the asyncio loop.
Cancel and await each observer **before** stopping its VM; `read()` drains even
on repeated cancellation, and `check()` preserves failures that would otherwise
be hidden by cancellation. Kernel reads cannot be killed by Python: retain a
finite external service guard and independent physical shutdown audit.

`collect_to_log` records queue/read/schedule latency. Its read duration excludes
serialization and persistence; do not interpret it as total worker time.
`measured_wait(backend, seconds, op, call, emit)` timestamps pause worker dispatch,
the full planned sleep, wake lateness and resume dispatch. `call` is the
controller's async, bounded blocking-call adapter. `emit` must be thread-safe
(it is also invoked from workers), include monotonic/wall timestamps, and avoid
blocking the controller. Failed pause does not claim sleep/resume success; the
owner retains responsibility for cleanup on any exception/cancellation.
`monitor_loop` records timer lateness and persists through the sampler.

The historical UKSM controller additionally skipped an offload-only process
scan, keeping STOP/ack/CONT and final audits. That backend is not present in the
current single-task runner; this PR does not enable offload or remove checks
from a current backend. Historical implementation changes are preserved in
`sampling-controller-diagnostic.diff` for review, not as an applicable patch.
The helpers require explicit adoption by a concurrent deployment.

Engineering evidence: `sampling-evidence.json`. These single-run results cover
the combined deployment changes, not an isolated speedup of these extracted
helpers. The AgentENV reference used the old collector. Preparation is excluded;
parent-cgroup usage includes charged services/cache, not proof of external cache
attribution. Original commands, sleeps, admission and deadlines were retained.

```sh
mise run test-py -- discover -s benchmark/tests -p test_resource_sampler.py
mise run test-py -- discover -s benchmark/tests -p test_wait_timing.py
```
