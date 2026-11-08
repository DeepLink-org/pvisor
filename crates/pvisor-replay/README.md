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

Stdin backpressure and early-close fixtures re-execute the native test binary,
with an independent alarm to bound a broken supervisor. They do not launch a
Python interpreter or PATH shim inside the short supervision deadlines. Readiness,
timeout classification, output capture and child reaping remain asserted.
The cooperative cancellation fixture covers pending-stdin EOF on wall/idle
timeouts, log quotas and SIGINT step limits. Wall/idle cases allow three seconds
for native startup; this tolerates cold starts but is not a readiness handshake,
so startup starvation beyond that budget can still fail the test. Its independent
alarm exceeds the wall deadline plus the longest termination grace.

The Linux leader-exit fixture covers incomplete stdin delivery when a descendant
keeps the read end open, both with and without inherited stdout/stderr. A native
pipe handshake establishes the descendant before the leader exits. The executor
must reject delivery promptly based on leader exit, not wait for a broken pipe
or a timeout, and kill the remaining group. The test temporarily adopts orphaned
children as a subreaper, verifies SIGKILL, reaps its descendant, and restores the
prior subreaper setting. An independent ten-second descendant alarm bounds a
broken cleanup path; the asserted completion budget is three seconds.
