# pvisor-replay

Implementation guards scan every Rust source recursively and exempt only the
crate-root `api.rs` and `lib.rs`. Negative fixtures verify that nested files
with those names and newly added adapters cannot add public inherent methods.

The crate root denies direct `unwrap`/`expect` in non-test Rust code. Test
assertions and non-panicking fallbacks remain allowed.

Replay preparation, tool execution and agent continuation for the
`pvisor-replay` companion. The engine validates recorded trajectories and
workspace boundaries before continuation; protocol-specific adapters remain
responsible for agent versions, history and resume semantics.

## Public API

Import contracts exclusively from `pvisor_replay::api`. `ReplayConfiguration`
loads TOML and converts it into an owned `PlaybackRequest`; `ReplayRequest`
loads versioned JSON and consumes the request with `execute`, returning
an `ExecutionReport` or `ReplayError`. Import the traits to call their methods.
`AgentProfile` provides adapter identity; `ReplayErrorClassification` classifies
`ReplayErrorKind`. Read the error message and retained locations directly from
`ReplayError`; no second accessor or classification path is exported. Public records and method declarations live
in `api.rs`; adapters, validation, process supervision and journal ownership
remain private. Root-level exports have been removed.

Loading performs no workload execution. Replay execution requires caller-provided
isolation, holds an exclusive state lock and refuses an existing output child.
Errors can retain partial effects; unresolved start receipts block retry.
The API declarations are uniform; native process supervision currently targets
Unix hosts. Agent versions and external model acceptance require separate tests.

`tests/api_boundary.rs` recursively rejects public implementation exports,
undeclared inherent methods, conditional API declarations and API method bodies.
External contracts exercise defaults, loading, validation, errors and execution
through `api`; existing lifecycle and recovery tests use the same entry point.

| Contract | External API coverage |
| --- | --- |
| Loading/defaults | `api_contract`: TOML unknown fields, absent files, JSON schema/agent/mode matrix; no workload or state publication |
| Errors/adapters | `api_contract`: every error category, exit code and retry classification; all adapter names and versions; owned diagnostic locations |
| Execution/lifecycle | `replay_contract`, `continuation_failures`, `sdk_recovery`: preparation, replay, continuation, durable admission, ambiguous effects, publication failures and cleanup |
| Exposure | `api_boundary`: sole entry, private implementations, declaration-only API, uniform declarations and the exact public behavior inventory |

## Shared adapter outcomes and SDK validation

Outcome prompt metadata accepts a JSON object map, so scalar metadata cannot
reach its insertion path. Adapter-specific fields remain intact; prompt facts
retain their hash, Unicode character count, injection count and mode-specific
reason without copying prompt text into the diagnostic record.

SDK observations are selected from the runner's configured path: Mini and Pi
read separate observation files, while SWE reads its trajectory. Runner step
counts use checked integer conversion and checked total-budget addition.
Invalid output does not publish `continuation_finished` or clear the existing
execution fence. The shared metadata and SDK-runner functions deny direct
non-test `unwrap`/`expect`.

`tests/sdk_validation.rs` drives all three SDK paths with local protocol fixtures,
not real SDKs or models. It checks successful results, malformed and overflowing
counts, trajectory/count disagreement, missing observation files and blocked
retry after uncertain execution. Private fixed and generated tests cover prompt
metadata and budget arithmetic.

## Codex rollout staging

Codex rollout staging uses the shared atomic JSONL writer to create parent
directories and replace the staged file. There is no separate parent-path
assertion or directory-creation path. Invalid session IDs remain configuration
errors; filesystem obstructions remain executor errors.

The dated rollout layout, timestamp precedence, recorded local offset and
legacy 1970 fallback remain unchanged. Fixed and generated private regressions
check exact paths, safe IDs, staged metadata and JSONL bytes, parent creation,
and retry after explicit removal of a filesystem obstruction. They launch no CLI
or model. Non-test `generic` adapter code denies direct `unwrap`/`expect`.

## Claude session and tool-result ownership

Claude parsing selects a session ID only when the observed set has one member.
Streamed assistant fragments retain the existing message-ID and stop-reason
rules. Terminal result selection uses the sorted active-chain tail without a
length-subtraction or unchecked final-element lookup.

Rebuilt result events require exactly one selected assistant owner and retain
their parent/source-assistant links. Missing ownership and mixed-owner results
remain trajectory errors. `tests/claude_preparation.rs` checks exact prefixes,
streamed/multi-call batches, result errors and source/router identity separation
through the public API. Invalid source cases publish no run state and can be
explicitly repaired when no prior state exists. No CLI or model is launched.

## Pi trajectory preparation

The Pi adapter retains validated tool IDs, names and object arguments as typed
parser output, without unpacking its own normalized JSON again. Empty native IDs
and omitted arguments (`{}`) remain accepted; observations match calls by ID,
including when results arrive in a different order.

Before writing the prepared prefix or appending `session_rebuilt`, the adapter
checks the native event array, selected batch, index conversion and prefix bounds.
Invalid internal plans return `Internal`; malformed tool fields retain `Trajectory`
and unsupported tool names retain `UnsupportedVersion`. The module denies
non-test `unwrap`/`expect` through Clippy. Private boundary regressions cover
missing, negative, fractional and overflowing indexes without artifact or
adapter-event publication.

`tests/pi_preparation.rs` uses only the public API and launches no SDK or model.
Fixed and generated cases check exact logical prefixes, native arguments,
text-only turns, reordered results, truncated final lines and incomplete final
batches. Invalid trajectories publish no state/output roots; explicit source
repair can retry the same run ID when no prior run state exists. Existing ambiguous
receipts still block admission. PrepareOnly leaves the workspace untouched.

## OpenHands action boundaries

Source parsing and SDK output counting share the replayable-action classifier.
`run`, `read`, `edit`, `run_ipython` and `think` count as actions; non-agent
records and `system`, `finish` and `message` do not. Unsupported agent actions
retain `UnsupportedVersion`.

The selected boundary must contain one action with an `i64` native ID and
identify a replayable agent event. Missing batches, invalid IDs and malformed
native containers return `Internal` before publishing prepared events or
`session_rebuilt`. The module denies non-test `unwrap`/`expect` through Clippy.

SDK output checks reject missing replay batches before ReplayOnly boundary
crossing, then enforce the total `max_steps` budget. `None` adds no replay-side
total-action budget. Invalid output does not publish `continuation_finished`.
Log setup retains the existing start-fence ordering: failure after
`continuation_started` leaves unresolved continuation state and blocks retry.

`tests/openhands_preparation.rs` checks exact selected native events, arguments
and causally late observations through the public API, without an SDK or model.
Fixed and generated cases verify that PrepareOnly executes no tools and leaves
the workspace and source unchanged. Private regressions cover malformed
boundaries, signed-ID limits, output budgets and retained continuation fences.

## Mini SWE Agent prefix boundaries

The parser retains validated call IDs, arguments and native actions in typed
records. Normalized call metadata, explicit empty IDs, generated fallback IDs
and the two native action encodings retain their existing representation.

Model-turn counting and preparation use checked native message indexes,
integer conversion and inclusive-prefix bounds. Missing batches or message
arrays and invalid boundaries return `Internal` before prepared-file or
`session_rebuilt` publication. PrepareOnly includes all selected batch results;
live replay stops at the selected action so the SDK can reconstruct fresh results.
The module denies non-test `unwrap`/`expect` through Clippy.

`tests/mini_preparation.rs` exercises multi-call batches through the public API
with both native actions and object/string tool arguments. Exact prefixes,
fallback IDs and text-only model turns are checked with fixed and generated
inputs; no SDK or model is launched and the workspace stays unchanged.

## SWE Agent history and assets

Encoded `replay_config` strings are decoded by matching the JSON string variant.
Object configs and asset resolution retain their existing behavior, including
relative-path validation, canonical root checks and UTF-8 requirements.

Preparation checks the trajectory prefix and the number of assistant history
messages before publishing an artifact or `session_rebuilt`. Invalid internal
boundaries return `Internal`; valid prefixes retain non-assistant history through
the selected assistant message. The module denies non-test `unwrap`/`expect`.

`tests/swe_preparation.rs` checks exact native prefixes, call-ID fallback,
encoded/object configs and file-backed problem text through the public API.
Fixed and generated inputs require no SDK or model and leave source files and
the workspace unchanged. Rejected source and asset cases publish no run state;
private regressions check both PrepareOnly and live-replay prefix rejection.

## Interrupted tool execution

Replay admission holds an exclusive run lock and recovers incomplete journal
tails before checking execution receipts. A new tool start invalidates any
earlier finish for the same native call ID, including IDs reused across turns.
An unresolved start remains ambiguous after run failure and blocks retry in
that state directory; use a new sandbox and run ID rather than silently
repeating uncertain side effects.

Native agent/SDK continuation also has a durable start/finish fence, because
its subprocess can execute tools without host-owned per-tool receipts. A start
without a validated finish is ambiguous even after a terminal run failure;
changing only the output root cannot authorize repeating those effects. Failed
continuations report `ambiguous_execution` and are not retryable in the same
state. Fully validated continuations remain eligible for explicit replay.
Existing journals with an unresolved continuation marker also fail closed;
operators must use a fresh sandbox and run ID rather than clear the marker.

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

Process capture finalization joins both readers before propagating stdout or
stderr failures, including redirected-tail read errors and reader panics.
Missing stdout capture state returns `Internal`, not a panic or empty success.
Redirected tail bytes, total length, truncation and stdout-first error precedence
retain their existing behavior. Non-test `process` code denies direct
`unwrap`/`expect`.

Fixed and generated tail tests cover zero retention, exact bounds and binary
data. A gated reader regression checks completion before error return. A native
child removes its redirect file before exiting; finalization must report an
executor error, retain the stderr log, reap the child/group and allow a healthy
successor using the same redirect path. These checks do not change supervision
deadlines.

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

Claude resume cleanup uses checked slice tails for boundary messages and closure
blocks. Canonical JSON encoding propagates serialization errors through
the existing hash/manifest results, without substituting empty text or digests.
Key ordering, string escaping, UTF-8 text, number rendering and manifest versions
are unchanged.
Fixed wire and digest regressions include a Python UTF-8 reference vector;
generated cases check nested values, first/later request cleanup, unchanged input
and rejection of modified observations. Non-test code throughout `bridge`
denies direct `unwrap`/`expect`; host startup and shutdown policies are unchanged.

## Validation

Runnable `api.rs` doctests cover configuration defaults and conflicting modes,
JSON validation before publication, and local PrepareOnly execution with a
recorded tool that must remain unexecuted. They require no agent SDK or model.


```sh
mise run test -- pvisor-replay
mise run test-doc -- pvisor-replay
RUSTDOCFLAGS='-D warnings' mise exec -- cargo doc --locked -p pvisor-replay --no-deps
```

Local fixtures cover protocol rewriting, credentials, stalled upstreams,
startup failures, cancellation and bounded teardown. These are not live-model
or external agent-version acceptance tests.

The Bash output fixture uses a finite 6 MiB writer and a 30-second execution
budget, asserts successful exit and truncation, and checks the full log length.
It does not measure CI output throughput. Timeout classification retains its
separate 50-millisecond fixture, and background-descendant cleanup remains a
separate assertion with its original two-second command deadline.

The short Bash and foreground process-group timeout fixtures reserve all nextest
slots, because their outer wall-clock assertions include synchronous process and
reader-thread startup before the execution watchdog begins. Concurrent cold
launches on hosted macOS can consume that setup budget. Their original execution
deadlines and total completion bounds remain enforced, with retries disabled.
The Bash fixture prints its PID and execs native `/bin/sleep`, then checks timeout
classification, readiness, reaping and absence of the original process group.

The OpenCode continuation failure/step-budget fixture also reserves all nextest
slots: its 15-second upstream readiness deadline includes the child harness,
shell probes and Python HTTP client startup. Its request deadline, failure-event
assertions and step-budget checks remain unchanged; retries remain disabled.

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
