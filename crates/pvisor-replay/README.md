# pvisor-replay

Replay preparation, tool execution and agent continuation for the
`pvisor-replay` companion. The engine validates recorded trajectories and
workspace boundaries before continuation; protocol-specific adapters remain
responsible for agent versions, history and resume semantics.

## Interrupted tool execution

Replay admission holds an exclusive run lock and recovers incomplete journal
tails before checking execution receipts. A new tool start invalidates any
earlier finish for the same native call ID, including IDs reused across turns.
An unresolved start remains ambiguous after run failure and blocks retry in
that state directory; use a new sandbox and run ID rather than silently
repeating uncertain side effects.

Replay observations require object payloads; tool start/finish receipts also
require a string call ID (including empty IDs accepted by native adapters).
Admission and diagnostic reads reject malformed
receipts with an ambiguous-execution error rather than panicking or silently
ignoring an uncertain operation. Rejection does not repair complete records:
explicit owner restoration is required, and failed admission releases its locks.

Artifact-publication failures report completed operations from the latest run's
durable finish receipts, including separate operations that reuse a native ID.
Fully resolved terminal runs allow an explicit rerun into a fresh output root;
this repeats tools and is not an exactly-once guarantee. An existing output
directory is never silently reused.

Supervised process timeouts and output draining remain active while configured
stdin is being delivered. Pipe backpressure does not suspend wall or idle
deadlines. Successful delivery closes stdin to signal EOF; write failures remain
executor errors, and timeout termination does not claim that all input arrived.
Cancellation closes any pending stdin before signaling the process group, so
cooperative children can drain buffered input to EOF and flush output during
their termination grace period. This applies to wall/idle deadlines, log quotas,
and step limits; it does not resume delivery of the remaining input.

## Local continuation bridges

Claude, Codex and OpenCode use private loopback bridges. Their protocol handlers
retain separate routing, nonce/manifest validation, sequence state and final
success checks. A shared private host owns listener startup, cancellation,
thread/runtime teardown and loopback proxy exclusions. Each bridge gets a fresh
local credential; upstream clients bypass ambient HTTP proxies. Child credential
and base-URL environment variables remain agent-specific.

Startup waits at most ten seconds for readiness. Explicit finish and Drop both
cancel local upstream waits and stop accepting connections. The server allows
one second for graceful draining, then drops its runtime and reports a
continuation error if draining failed. The outer worker stop budget remains five
seconds; an unconfirmed worker is reported as detached, never as a clean finish.
Drop is best-effort; callers use explicit finish to observe teardown failures.
Cancellation does not revoke a model request already received by the upstream.

Claude owns its retry/status policy and uses a two-thread runtime. Codex also uses
two runtime threads; OpenCode uses a current-thread runtime. A failed or
cancelled transport exchange does not increment the forwarded-request count;
protocol-specific finish checks still decide continuation validity.

## Validation

```sh
mise run test -- pvisor-replay
```

Local fixtures cover protocol rewriting, credentials, stalled upstreams,
startup failures, cancellation and bounded teardown. These are not live-model
or external agent-version acceptance tests.

The Bash output fixture uses a finite 6 MiB writer and a 30-second execution
budget, asserts successful exit and truncation, and checks the full log length.
It does not measure CI output throughput. Timeout classification retains its
separate 50-millisecond fixture, and background-descendant cleanup remains a
separate assertion with its original two-second command deadline.

Stdin backpressure and early-close fixtures re-execute the native test binary,
with an independent alarm to bound a broken supervisor. They do not launch a
Python interpreter or PATH shim inside the short supervision deadlines. Readiness,
timeout classification, output capture and child reaping remain asserted.
The cooperative cancellation fixture covers pending-stdin EOF on wall/idle
timeouts, log quotas and SIGINT step limits. Wall/idle cases allow three seconds
for native startup; this tolerates cold starts but is not a readiness handshake,
so startup starvation beyond that budget can still fail the test. Its independent
alarm exceeds the wall deadline plus the longest termination grace.

The ignored `slow_progress_then_idle_cancellation_drains_and_allows_a_fresh_process`
check exercises real lifecycle timers for about three minutes. In both pipe and
redirected-stdout modes, a native child emits stdout/stderr progress at 20-second
intervals before stalling with pending stdin. The 30-second idle watchdog must
refresh on both streams, then cancel, deliver stdin EOF, capture cooperative
shutdown output and reap the child/group. A fresh process must then succeed.
The child has an independent 150-second alarm; the owner has a 120-second wall
deadline per mode. Run it explicitly with:

```sh
mise run test -- pvisor-replay -- --lib --run-ignored only \
  -E 'test(slow_progress_then_idle_cancellation_drains_and_allows_a_fresh_process)'
```

This is a low-load timer/cleanup contract, not live-agent continuation coverage
or a throughput benchmark.

The Linux leader-exit fixture covers incomplete stdin delivery when a descendant
keeps the read end open, both with and without inherited stdout/stderr. A native
pipe handshake establishes the descendant before the leader exits. The executor
must reject delivery promptly based on leader exit, not wait for a broken pipe
or a timeout, and kill the remaining group. The test temporarily adopts orphaned
children as a subreaper, verifies SIGKILL, reaps its descendant, and restores the
prior subreaper setting. An independent ten-second descendant alarm bounds a
broken cleanup path; the asserted completion budget is three seconds.

`process::generated_tests` adds a finite full-duplex matrix without a shell or
interpreter. A native child emits 128 KiB+1 stdout and 64 KiB+1 stderr before
consuming stdin, closes stderr, receives stdin through EOF and echoes the exact
received bytes to stdout. The parent checks independently held input bytes,
bounded channel-tail windows (including zero and pipe/read-chunk boundaries),
redirected stdout, log size/permissions and ordinary nonzero exits. A deliberate
early stdin-close error is followed by a fresh healthy native process; that
composition does not reclassify the already-covered stdin-close path as a new
defect. Each child has a five-second wall deadline and independent eight-second
alarm, with no descendants. The generated check does not intentionally expire
deadlines; timeout/cancellation coverage remains in the separate existing tests.

Default coverage is 20 seeds. `PVISOR_CHAOS_FIRST_SEED` and
`PVISOR_CHAOS_SEED_COUNT` select a checked finite interval (1–4096 seeds per
invocation). Every seed runs four child lifecycles sequentially, with input at
most 1 MiB+1 and bounded output. Failed private `replay5-*` fixtures retain seed,
PID, received-input and log evidence; success cleanup follows reaping/group
checks and assertions. This is process supervision coverage, not replay
exactly-once execution, live-agent acceptance or an adversarial process escape
test.
