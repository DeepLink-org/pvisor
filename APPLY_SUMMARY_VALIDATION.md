# Integrated apply-summary validation

2026-11-08 UTC. Sole test/build slot released at **02:29:31**.
No production edits, builds, commits, pushes, Git merges or publication.
Read root `README.md`/`AGENTS.md`, `plugins/README.md`, and campaign scope,
safety rules and prior plugin-round evidence before execution.

## Snapshot and results

The inherited snapshot included apply summaries, all earlier plugin chaos
regressions, and round7's `--stdio inherit` output repair.

| Snapshot | UTC | Exact task | Actual result |
| --- | --- | --- | --- |
| Before review followup | 02:26:35–02:26:58 | `mise run test-agent-plugins` | 61 passed; 22.723 s |
| Before review followup | 02:26:58–02:27:30 | `mise run test-agent-plugins-native` | 36 passed; 31.577 s |
| After review followup | 02:28:03–02:28:25 | `mise run test-agent-plugins` | 65 passed; 22.571 s |
| After review followup | 02:28:25–02:28:56 | `mise run test-agent-plugins-native` | 36 passed; 30.806 s |

All four commands exited zero, with zero failures, skips or cancellations.
These are actual unfiltered test counts, not unmatched file-wrapper counts.
The first two passes are **pre-review-followup evidence**, not final feature
approval. Parent then explicitly authorized merging and validating the correction.
`merge_worktrees` cleanly pulled only the summary implementation/test followup:

- Source thread: `ksQQfj1nkaiaSOWJIeu_dhVHMZLOABvN75IBxBDuNWdurkVNCYkuySUr0iCB`
- Worktree: `4ca22f22-7269-43e4-b909-0a32154d70dc`
- Exact frontier: `3c2b18e6-6c65-46b8-89a2-97b8e2841468.1844468`

Final coverage includes non-string action-kind refusal in all inventories,
Pi/DSH malformed-kind refusal before approval, all Unicode bidi controls,
readable selected/required/excluded/blocker summaries, malformed/truncated/
blocked refusal, real native add/modify/delete/mode/binary candidates, and
real Python unittest/Git/binary-output regressions. Native preview does not
classify binary content or report previous modes; tests require factual
limitations rather than invented classifications/deltas.

## Execution and resources

Each exact task ran through `nice -n 10 timeout -k 30 120`.
An owned PATH shim dispatched test invocations as
`/usr/bin/node --test --test-concurrency=1`, preserving task file arguments.
Process-local settings:

```text
MISE_TRUSTED_CONFIG_PATHS=<exact child worktree>
MISE_YES=1 MISE_AUTO_INSTALL=0
CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 NEXTEST_TEST_THREADS=1
PVISOR_PLUGIN_TEST_PYTHON=/usr/bin/python3
```

Durable owned TMPDIR/output roots were `.asv8/tmp` / `.asv8/fixtures` for
baseline and `.asv9/tmp` / `.asv9/fixtures` for final validation.
Full untrimmed task output and resource JSONL remain in each evidence root.
Runner/shim and independent inventory script are under `.asv8/`.

All eight pre/post workload gates passed:
MemAvailable 18,487,283,712–19,107,549,184 bytes;
free disk 342,891,868,160–343,059,918,848 bytes;
load1 0.866–2.210. Required thresholds were >=6 GiB RAM,
>=20 GiB disk and load1 <=8.
Final post-inventory resources at 02:29:30:
19,091,820,544 bytes RAM, 342,880,083,968 bytes disk, load1 1.501.

Reused read-only binary, verified before and after execution:

```text
/home/reiase/workspace/pvisor/.delta/worktrees/c3re3k2mzkkm/pvisor/target/debug/pvisor
SHA256 6da4ef6844ed2f36a9d7c0cc2172c77b576b99c0e354a296515b855785960f20
```

## Cleanup and retained evidence

Inspected retention controls before execution. Setting
`PVISOR_PLUGIN_TEST_OUTPUT_DIR` deliberately retains documentation fixture
roots; `summary.native.mjs` deliberately retains its candidates without drop.
Other native fixtures performed their own guarded reconciliation/drop/removal.
No unknown/live fixture was deleted. No independent destructive cleanup ran.

Independent `/proc` cmdline/cwd and readable same-UID mountinfo inventory found
**zero owned process matches and zero owned mount matches**. Three same-UID
process entries denied inspection: 3718, 3720, 696685; no unrestricted
all-process/all-namespace absence claim is made.

All 28 retained native task records were queried using bounded read-only native
`status SELECTOR --json`: every command exited zero and every Session was
terminal/nonlive (22 completed, four failed, two cancelled).
Overlays: 24 discarded, four staged. The four staged records are the two
summary fixtures with intentionally pending candidates and the two empty
documentation successor stages. Full identities, raw status output, exact
selectors and retained-root inventory are in `.asv8/inventory.json`.

Retained fixture directories, relative to this child worktree:

```text
.asv8/fixtures/plugin-native-09QP0y
.asv8/fixtures/plugin-native-IE8qvY
.asv8/fixtures/plugin-native-IGQIWj
.asv8/fixtures/plugin-native-K6dvCt
.asv8/fixtures/plugin-native-RMMvgl
.asv8/fixtures/plugin-native-TFNXio
.asv8/fixtures/plugin-native-XJ6NwQ
.asv8/fixtures/plugin-native-b7oRpT
.asv8/fixtures/plugin-native-cT4m0A
.asv8/fixtures/plugin-native-gGwLQh
.asv8/fixtures/plugin-native-vmsETd
.asv8/fixtures/plugin-native-xZV7xV
.asv8/fixtures/plugin-summary-tSRVbh
.asv9/fixtures/plugin-native-0Ce0wz
.asv9/fixtures/plugin-native-0pg7o5
.asv9/fixtures/plugin-native-4zBfVe
.asv9/fixtures/plugin-native-6mrJc0
.asv9/fixtures/plugin-native-G5N58J
.asv9/fixtures/plugin-native-P4NIdJ
.asv9/fixtures/plugin-native-QuOnoH
.asv9/fixtures/plugin-native-pEWMNd
.asv9/fixtures/plugin-native-qdTEEE
.asv9/fixtures/plugin-native-wgjc6D
.asv9/fixtures/plugin-native-ww9gB0
.asv9/fixtures/plugin-native-y9ExML
.asv9/fixtures/plugin-summary-2v92qc
```

Both evidence TMPDIRs also retain `node-compile-cache` and five disposable
`pvisor-agent-home-*` directories each. All are preserved, not swept.
Evidence lives in child checkout
`/home/reiase/workspace/pvisor/.delta/worktrees/9xkg8620d156/pvisor`;
runtime-generated fixture directories may remain disk-local rather than replicated.

`git diff --check` passed. No test failures or automatic retries occurred.
Pi/DSH behavior is callback-contract coverage, not live interactive host UI.
No Rust/workspace/project Python suite, compiler, rustdoc, macOS, VM/OCI,
cloud-model, OS/network-change, install or semspec operation ran.
Real Python workloads inside native plugin tests are distinct from running
the repository's Python suite. Final independent static rereview remains
parent-owned.

The portable task includes the repository's packed-plugin installation fixture:
it uses a disposable npm prefix/cache and a loopback-only registry, not a host
installation or dependency download. “No install” above means no additional
host tool installation; it must not be read as excluding that fixture.
The parent subsequently completed the independent static rereview without
findings. Machine-specific `.asv8` runner/inventory files were not retained in
the parent worktree; the original child evidence remains at the path above.
