# Bounded seeded native stage correctness driver

`stage_chaos.py` drives **real native Linux/KVM** stage/apply/drop commands. It
never creates a synthetic ledger, fake stage, product failpoint or approved
snapshot. It checks stage correctness under bounded fault injection.

## Run and supervise

Use a non-root Linux user with `/dev/kvm` access, a built native executable and
its matching firmware. No sudo, `apply --rootfs`, network traffic or writes to
real host configuration are used. Build matching firmware with:

```sh
mise run fw -- build -j2
```

The output must be a **fresh** directory whose existing ancestors are owned by
root/current UID and not group/world writable. Symlink ancestors and sticky
`/tmp` are deliberately rejected for executable storage; repository `artifacts/`
is suitable. Never chmod an existing ancestor to bypass safety checks.

Use the firmware path printed by the build. For a bounded run:

```sh
python3 -m tests.stage_chaos \
  --binary target/debug/pvisor \
  --firmware /absolute/path/to/matching/libkrunfw.so.5 \
  --output artifacts/stage-chaos-run \
  --seed 314159 --seconds 10800 --rounds 40
```

Every invocation needs a different output, including after a product rebuild.
The driver copies and hashes the executable and firmware (`pvisor` and
`libkrunfw.so.5` in the output root), records Git HEAD/dirty status, configuration
and a monotonic deadline. Git metadata does not prove which source produced an
older executable: artifact SHA-256 is the binary identity.

Arguments: required `--binary --firmware --output --seed --seconds`; optional
`--rounds`, `--scenario`, `--memory 128m|256m|512m` (default 256m), and
`--max-evidence-mib` (default 1024, range 1–8192). Seconds must be finite and in
(0,86400], rounds in [1,1000000]. One VM at a time, one vCPU, network none;
concurrent apply/drop commands do not create another VM.

Default schedule: scenarios 1–10 below repeated four times form a named
40-round cycle; the cycle then repeats until the budget/round limit. Seed and
round determine payloads, order and tiny injection delay; native scheduling is
not deterministic. Use `--scenario NAME --rounds 1` to select a single scenario.
No skip/unsupported outcome counts as a pass.

The driver prints a flushed JSON progress line about once per minute while
watching subprocesses and at case boundaries. Default behavior stops at the
first assertion, command timeout or safety failure. A time-only run stops between
complete cases when less than 60 seconds remain, recording `budget_complete`
rather than starting a case that cannot receive its full bound. Actual elapsed
time, not the requested budget, remains authoritative. Exit status is 0 for all
finished executions passing, 1 for failure/abort, and 2 for finished runs containing
misses or unknown-verified outcomes (not passes). A missed injection need not
stop a longer run, but remains visibly incomplete in its final exit/summary.

## Scenario contracts

1. **chain**: real workload adds/modifies/deletes/renames multiple files, writes
   an outside-project sibling sentinel, then exits 23. Host snapshots must be
   unchanged. Preview all/exclude/paths (space, Unicode, literal glob characters),
   reject `../` and absolute selections, selectively apply, check exact host
   content/type/mode/file set, verify unselected retention, apply again, drop and
   repeat drop without undoing applied files.
2. **conflict**: external target edit must block apply without overwrite or
   ledger/upper writes. Existing `session.lock` owner bookkeeping (body/size,
   mtime/ctime) may refresh or be cleared on *execution* refusals after a kill;
   inode, owner and permissions must stay identical. Previews have no exception.
3. **symlink**: prove nested-file preview ready before replacing its ancestor
   with an outside-pointing symlink; execution must refuse without escape.
4. **rebind**: replace the launch project directory. Apply must refuse; original
   renamed project, replacement project and stage must remain unchanged.
5. **project_all**: apply all remaining project changes and require exact target
   result and `in_scope=0`. Outside-scope leftovers may be dropped. Only a real
   `outside_scope=0` exercises fully-applied drop refusal; project-all alone does
   not establish that coverage.
6. **nested_all**: modify an existing nested file as well as ordinary files; an
   all-project preview must be ready with the exact path set.
7. **parallel_apply**: launch two real applies of **one file**, drain both and
   retain bounded per-command logs while watching. Require observed process
   overlap, one applied winner, at most one committed ledger record and an exact
   complete new target. The other result may be evidenced lock refusal or a
   serialized `no_changes`; duplicate successful mutation claims fail.
8. **parallel_drop**: seeded launch order for one-file apply versus drop. Exact
   old target requires a successful drop and no apply ledger; exact new target
   requires a single successful applied result and committed ledger. Both may
   succeed in apply-then-drop order without undoing the target. Refusals must
   carry lock/terminal-state evidence. Unobserved overlap is **miss**, not pass.
   This asserts no multi-file atomicity, which the product does not promise.
9. **live_cancel**: stage writes its project/sentinel changes, prints a unique
   ready marker and sleeps. Require both actual guest marker and a running run
   record, then refuse live apply/drop. SIGTERM only the registered frontend
   PID/starttime via pidfd; wait and clean owned descendants within 30 seconds.
   Inspect the actual run/completion/upper evidence. Matching terminal IDs,
   timestamps and completion binding give a *known committed* branch, which must
   review successfully before drop. Missing/inconsistent/unknown completion is
   **not** assumed unchanged or cancelled: apply/drop must refuse; the complete
   case is retained as `unknown_verified`, not counted as a known lifecycle pass.
   Host isolation is independently checked, not used to infer guest outcome.
10. **kill_apply**: native workload modifies 64 existing files × 64 KiB (4 MiB,
    not a giant allocation). Poll the real `apply-ledger.json` at most 30 seconds.
    Only after observing `prepared`, a seeded 0–2 ms delay, re-observing the same
    Prepared apply ID and verifying an owned PID/starttime does pidfd SIGKILL
    count as **injected**. Otherwise it is **miss**, never fault pass. Preserve
    the gate records, final Prepared/TargetApplied/Committed ledger, target and
    upper snapshots and logs. Pending preview/drop must refuse without changing
    ledger/target; Prepared must retain upper payloads. Multi-file target state
    may be partially published: each named file must be complete old/new, not an
    invented all-or-none batch oracle. The product's private `.pvisor-apply-*`
    publication debris is recorded, not blindly deleted. A kill can race past
    the gate to Committed: then all payloads must agree with Committed, and no
    pending/recovery coverage is claimed. No automatic recovery apply is run.
11. **project_only** (explicit `--scenario`, not in the default cycle): perform
    the ordinary selective-apply lifecycle without the outside-project write.
    Applying the remaining project changes must leave both `in_scope=0` and
    `outside_scope=0`, preserve the exact expected project tree and make drop
    refuse without changing the accepted project or environment. Unexpected
    environment candidates fail rather than being treated as full application.

All JSON previews recursively snapshot the whole project/stage including file
hash, mode, UID/GID, device/inode, mtime and ctime; atime is ignored. Text
`--dry-run --diff` is separate (`--diff` conflicts with `--json`). A refusal is
not treated as proof of a particular internal check: blocker text is evidence.

## Process ownership, bounds and artifacts

Linux `PR_SET_CHILD_SUBREAPER` tracks double-fork/session-separated owners, not
just the frontend process group. The driver records explicit Popen identities,
observes their descendants and newly adopted children, and excludes processes
present at scope creation. A scope is exclusively owned by this driver; do not
embed it in an unrelated subprocess-launching multithreaded host. Signals require
owned PID + Linux `/proc` starttime + a pidfd verified against that starttime.
No name matching, `pkill`, broad killpg or signalling unrelated host processes.
The libc pidfd API works even when the Python build omits pidfd wrappers.

Cleanup records actual signals, elapsed, survivors and **complete** status;
Popen reaps its direct children and the subreaper reaps adopted descendants.
TERM then KILL escalation handles leaked owners. An incomplete cleanup stops the
run before another VM. Final cleanup also runs on failure, SIGINT and SIGTERM.
SIGKILL/power loss cannot guarantee a summary. Failed/unknown/pending cases are
never automatically applied, dropped as success or deleted. Operators should
inspect recorded survivors before manual cleanup of their own evidence roots.

Every product subprocess receives an explicit environment dictionary, never the
shell environment: private HOME/XDG/config/cache/runtime/runstore/TMPDIR, fixed
PATH/locale and frozen firmware search path. No credentials/proxies are inherited.
Host-rootfs reads remain possible; this is not a confidentiality test.

Commands are bounded to 30 seconds, each case to 60 seconds, clipped to the global
monotonic deadline. Ready observation is at most 20 seconds. Safety cleanup can
add bounded overhead (at most 30 seconds per cleanup; normal/final cleanup uses
5). Synchronous hashing/snapshot/copy and filesystem I/O are not hard real-time.
stdout/stderr memory and watched log files are each capped at 32768 bytes per
command while excess output is drained and counted; truncated JSON never passes
an oracle. Artifact JSON reads are capped at 2 MiB. JSONL has a configurable cap
with small reserved abort/cleanup diagnostics beyond it.

`evidence.jsonl` contains scenario/seed/round, argv, PID identities, exit/JSON,
bounded output, timing, expected/actual oracles, launch overlap, readiness,
completion branch, Prepared gate and real ledgers. `summary.json` separates
attempted/finished/passed (`completed_rounds`) counts, per-scenario passes,
miss/unknown outcomes, injections, retained paths and cleanup. `owned_signals`
includes explicit fault signals, not just cleanup escalation. Sum actual
`active_elapsed`, not requested seconds, for soak accounting; report wall time,
revision, failures and aborted runs separately.

Successful ordinary/race/known-cancel cases are fd-safely removed after guarded
ownership/direct-child checks and complete process cleanup. Every kill-apply
execution (including misses), unknown branch and failure is retained. JSONL,
frozen inputs and summary always remain. The JSONL cap does not bound all retained
VM backing files: provision disk, monitor `retained_cases`, and reconcile owned
roots. Never traverse upper symlinks for manual cleanup.

Not yet covered: explicit owner/runner-specific SIGKILL, power loss and automatic
recovery correctness. Unknown and kill-raced-Committed branches are implemented
but only actual per-run branch evidence establishes their execution. A Prepared
window miss is not unsupported, injected or pass.

## Driver checks

```sh
mise run test-py -- tests.unit.test_stage_chaos tests.regression.test_stage_chaos -v
```

No VM is needed for safety/oracle tests, including real detached fake children,
starttime rejection, bounded capture/deadlines and incomplete summary accounting.
For a short native smoke, use the run command with
`--seconds 60 --rounds 1 --scenario NAME` and a fresh output.
