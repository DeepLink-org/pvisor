# Round18 offline integration and IPC validation

Three targeted Linux Host plugin cases passed on 2026-11-08,
05:28:41.833–05:28:43.249 UTC (~1.417 seconds). The sole TEST/BUILD slot was
released after cleanup audit at05:29:10 UTC. No workload was rerun.

## Contract and observed cases

Linux Host deny-all uses a private network namespace with its own enabled
loopback interface, not a prohibition on every socket. See
[`executor-boundaries.md`](docs/src/en/security/executor-boundaries.md) and
[`sandbox.rs`](crates/pvisor/src/executor/sandbox.rs). macOS Seatbelt has different
local communication semantics; these results are Linux-only.

[`plugins/test/offline-ipc.native.mjs`](plugins/test/offline-ipc.native.mjs)
reuses the existing coding fixture, one Session and one Python unittest per case:

1. `tempfile` create/read/remove, a bounded subprocess using the same interpreter,
   and an owned AF_UNIX socketpair passed.
2. A task-owned TCP loopback server/client inside the same sandbox passed.
3. One fixture-owned host TCP listener on ephemeral `127.0.0.1` passed its host
   positive control, while the sandbox's single connect attempt was denied.
   The host request count remained one. Only a synthetic read-only canary was
   served; no real host service, socket, credential or application state was used.

Each case checked staged-only `result.txt`, completed/nonlive status, review
and drop without publication. Existing coding-fixture cancellation/join and
exact-selector reconciliation guarded cleanup. The task server registered
cleanup before bind and joined its bounded thread. The host listener registered
cleanup before bind, destroyed and close-awaited owned sockets, then awaited
server close even on failure.

## Exact execution and evidence

```sh
nice -n 10 timeout -k 30s 120s /usr/bin/mise run test-agent-plugins-native
```

An owned Node PATH shim preserved mise's native filename glob and injected
`--test-concurrency=1 --test-name-pattern="^offline IPC Linux:"`.
Cargo jobs2 and Rust/nextest/math threads1 were set, with mise auto-install
disabled. Node was `/usr/bin/node` v24.22.1; the interpreter was
`/usr/bin/python3`. No install or build occurred.

The native command exited **0**. Node emitted its **specs reporter**, with
**three selected case passes plus13 unrelated file-wrapper passes**,16 total,
not16 exercised cases. The launcher subsequently exited **1** because its TAP
parser did not recognize specs-format output. Reading that same retained output
confirmed the three selected passes; this was a harness reporting failure, not
a native failure or a reason to rerun uncertain work. A future explicit-TAP
launcher correction was made statically and remains **unrun**.

Read-only binary:
`/home/reiase/workspace/pvisor/.delta/worktrees/c3re3k2mzkkm/pvisor/target/debug/pvisor`.
Its before/after SHA256 was
`6da4ef6844ed2f36a9d7c0cc2172c77b576b99c0e354a296515b855785960f20`.
The executed test source SHA256 was
`90344e76e21b321339043c3338a48e8aecdcd64562135f59f4cad77c70a47fb4`.

Local evidence is retained in `.asv8/round18-20261108T052841Z/`:
`native.tap` contains1362 bytes of specs-format output despite its filename,
SHA256 `f85c1f08f177870189ad36aaab5d50fd26667806a6b0dcd7262d3fb9eb356857`.
`events.jsonl` records UTC command, selected environment controls, source hashes,
resources and audit corrections. These local artifacts and the local launcher
are not publication deliverables; this root report records the outcome.

## Resources, cleanup and limits

Pre/post resource gates passed (RAM>=6GiB, workspace disk>=20GiB, load1<=8):
available RAM18,660,524,032 /18,424,524,800 bytes;
disk340,879,659,008 bytes and load1=0.3935546875 at both samples.
Short OS-temp scratch capacity was not claimed to exceed20GiB.

All three fixture roots were reconciled and removed; the short owned root
`/tmp/r18-jjtg_do5` was absent. An initial process token search matched its own
observer shell. The subsequent ancestry-aware audit found zero owned survivors,
zero denied/raced observations and no owned mount in the observer's mount view.
No process was signalled. This is not an audit of every mount namespace.

`node --check plugins/test/offline-ipc.native.mjs` and `git diff --check` passed.
No full aggregate or other-platform coverage is claimed. There was no external
traffic/DNS, cloud/model/paid call, host network setting change, policy weakening,
semspec change, commit, push, merge or publication. No product defect was found.
