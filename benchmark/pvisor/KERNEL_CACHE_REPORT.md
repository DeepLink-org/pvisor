# Final P1 source: noatime kernel-cache benchmark status

## Conclusions

**Final P1 source was newly frozen and built, and the complete five-condition noatime preflight and independent profile passed. Formal performance acceptance is BLOCKED: all three 30-sample timing attempts detected concurrent Cargo checks and were rejected. There is no accepted final-source P50, percentage change or bootstrap CI.** Partial batches are not combined and earlier performance figures are not substituted.

The previous noatime cohort is **limited performance evidence for its earlier source, not acceptance of the final P1 fixes**. Its report, CSVs, frozen build and raw evidence remain intact. The still earlier Btrfs/future-atime cohorts remain unaccepted historical diagnostics.

| Evidence | Final-source status |
|---|---|
| New frozen release build | PASS |
| Five-condition preflight, n=1/cell | PASS |
| Formal timing, 3 warmups + 30 samples/cell | BLOCKED; three whole cohorts rejected |
| Independent diagnostic profile, n=3/case | PASS; not elapsed-time performance evidence |
| Formal publication guard | Rejects all failed timing reports |
| Benchmark/publication tests | 26 passed |

## Motivation

The Core-owned copied-hardlink mapping and positional-copy append fixes change the source under test. An earlier run cannot validate a later binary, even if its fixture and cache policies match. Final performance evidence requires a new, complete, uncontaminated same-binary comparison; cached-read/invalidation correctness probes remain narrower than the implementation's dedicated regression tests.

## Experiment design

Registered **B-FS-ENG** for timing and separate **B-FS-DIAG** for counters. The measurement harness/driver and workload protocol are unchanged from the noatime experiment: one frozen release binary, CPUs `0,1`, seed `4207`, 3 warmups and 30 planned samples/cell; 2,048 byte-verified files in 32 half-deep branches. Conditions are native, legacy-writable (immutable physical cache on; default 1s), Metadata-writable60s, Metadata-readonly60s and MetadataAndData-readonly60s. Comparisons keep view semantics fixed: legacy versus writable Metadata, and readonly Metadata versus readonly KEEP_CACHE.

Each cohort uses a new private user/mount/PID namespace and genuine `noatime,nosuid,nodev,mode=0700,size=512m` tmpfs for all native/lower/upper/work inputs. Live mountinfo/device proofs and past-atime physical file/directory read checks are retained. The coordinator is PID 1, drivers/tools are contained descendants, no view/FD aliases are exported, and owner-only/default-permissions/real writable fusectl admission remain enforced. There is no journal, preimage, observation metrics, custom policy or exclusions.

Hot/TTL/readsearch have immediate untimed priming; the TTL-window waits 1.1s outside the operation timer and does not test 60s expiry. Verified tools check clean git status, exact rg paths and every input byte. Whole-tools launches a new driver/mount per sample and includes normal task/unmount/exit plus proof/supervision bookkeeping. Namespace setup and backing archival are outside task timers. Teardown writable mutation and readonly EROFS/namespace probes gate successful cohorts. They do not exhaustively test reclaimed late aliases, O_APPEND transitions, notification fault injection or the full crate contract suite.

Host-wide interference checks run approximately every 250ms outside the private namespace; inner checks run before/after cases. Any detected build/test rejects the entire cohort. Slow valid samples are not filtered, and failed samples never become zero latency or valid bootstrap draws. New directories are used for every attempt, with no overwrites or cross-batch pooling.

### Exact source under test

The identity is the **frozen dirty-worktree byte inventory**, not Git HEAD alone. HEAD context is `cd9827af8fa3a975e3b3974e38be747e91a097e2`. The frozen source has 1,121 files and contains:

- Core `copied_hard_link_metadata`, forwarded by `FilesystemService`.
- Host late-alias metadata/identity obtained from the Core-owned map, independent of reclaimable adapter inode mappings.
- `copy_file_range` calls `set_append(&output, false)` before positional copying.

The intake checks establish which source was built, not exhaustive semantic validation of those fixes. The snapshot also records other existing worktree changes, including OCI source; no cross-version causal comparison is claimed. `source-version.json` lists all differences from the previous noatime snapshot and binds the build receipt and three P1 implementation file hashes.

## Data and analysis

### Formal timing is unavailable

| Rejected cohort | Captured partial rows, including warmups | Interference witness |
|---|---:|---|
| `kernel-cache-p1-final-timing-20261007` | 369 | PID 884206: editor `cargo check --workspace --all-targets` |
| `kernel-cache-p1-final-timing-quiet-20261007` | 105 | PID 885428: Cargo process |
| `kernel-cache-p1-final-timing-isolated-20261007` | 656 | PID 894939: editor `cargo check --workspace --all-targets` |

The first retry followed 20 quiet seconds; the third followed 60 quiet seconds. Another task's `pytest ... test_lazy_startup.py` was observed while waiting, not used as evidence that a complete timing window was isolated. All three attempted cohorts remain FAIL. **Accepted final-source formal sample count is zero**, irrespective of the number of individually byte-verified rows captured before interference.

The current summary CSV is therefore an explicit **blocked-status table**: 25 planned condition/workload cells, 30 planned samples each, zero accepted samples, and empty latency/percentage/CI cells. It is not a successful performance distribution. No final-source speedup or slowdown can be inferred from this publication.

### Independent diagnostics

`kernel-cache-p1-final-profile-20261007` passed using the same final frozen binary. It contains 90 fresh diagnostic cases plus five persistent correctness processes, with 95 logs and 152 uniquely final profile instances. All cumulative records are retained; final counts replace earlier checkpoints rather than being summed. Frozen construction sites account for one core and one host-adapter profile per overlay; notifier and admission-probe threads construct no additional profile.

The counter CSV is generated only after full diagnostic validation: exact case membership, all final logs, input inventories, noatime backing/device proofs, containment receipts, retained backing archive and source/binary/harness hashes. It retains lifecycle medians/ranges and independently mounted prime-only deltas. **Its recorded elapsed times are not used for formal timing**, and its successful counts do not fill missing 30-sample performance cells.

### Failure containment and cleanup limitation

Normal preflight/profile receipts verify zero surviving namespace users and no owned mounts after ordinary FUSE/tmpfs detachment. Earlier simple-process namespace-init/unshare-death checks remain separate evidence, not a proof of immediate completion for every FUSE kernel wait.

The first two failed timing attempts left namespace init and exiting FUSE tasks after the outer supervisor killed unshare. Two driver task groups had a remaining thread in `request_wait_answer`; their leaders' mountinfo was already unavailable. **Namespace kill was not an immediate bounded-cleanup guarantee on this failure path.** The original failure receipts retain the observed survivors and were not rewritten to say clean.

Cleanup used the remaining threads' actual mountinfo to identify only this experiment's live connections. Exact endpoints 97, 99, 100 and 101 were aborted, with full mountinfo/operation receipts saved under `kernel-cache-p1-final-failed-cleanup-20261007/`. Those task groups/init PIDs subsequently disappeared. No unrelated FUSE connection was touched and no global flags or permission guards were relaxed. Host mountinfo had no exported kernel-cache mounts. The third failed cohort had no remaining namespace members at the later audit.

Failed tmpfs contents may be lost if outer termination happens before archival; durable protocol/stderr, partial reports, interference and containment receipts remain. This is another reason failed cohorts cannot be accepted. The cleanup/supervision failure path needs further harness hardening before relying on automatic failure cleanup alone.

### Publication, retained history and receipts

The final-source build/preflight/profile and three failed timing directories are new and separate. The previous successful noatime output directories are unchanged. Its public report/CSVs were archived before replacement under `.data/kernel-cache-noatime-pre-p1-history-20261007/`; `status.json` explicitly states `limited-performance-evidence-pre-final-P1` and `final_fix_acceptance=false`, with hashes of the prior files and raw receipts. No prior noatime or Btrfs percentages enter this final-source status table.

Publication adds an explicit blocked-report path, without relaxing the existing accepted-cohort validator. It independently verifies the successful diagnostics and final source-version receipt, confirms all attempted timing reports failed and are refused by the normal validator, and exports **no formal timing statistics**. The publisher/tests are frozen and hashed in the new publication receipt. The normal accepted-timing path still requires a complete passed 30-sample/3-warmup cohort and all prior hash/containment/noatime checks.

Files: [formal status CSV](kernel-cache-summary.csv), [verified final-source counter CSV](kernel-cache-counters.csv). Reproduction and exact output names are in the [README](README.md#extended-linux-host-api-kernel-cache). Local raw evidence is under `benchmark/pvisor/.data/kernel-cache-p1-final-*`; the blocked publication is `kernel-cache-p1-final-publication-blocked-20261007/`.

| Source/provenance receipt | SHA-256 |
|---|---|
| final frozen source inventory, 1121 files | `579575698f155226152c1e4f67c98bb8595d6940abd973324857c8834b36b213` |
| final release binary | `05e9310754456f67b7623e4b4cbec5c2bd3bdc0e5474d278d60f84891d7f4c8b` |
| build receipt | `1b7a753607f5aa3487b776154d363bf86047cfef6daf2de99691359a2a4a6ff3` |
| source-version receipt | `680bbeab4d994cb7604ea5b09a6d48439b9bf46b6a5745c83b5db3a91b06b228` |
| Core implementation | `956150234ab1c21dc82965302d5864737d4edb668cea076bda86b81c4cc245ef` |
| service implementation | `b31ea3d5b5ea5ea05780bbb9d63745b538985b18ed43e4ad843a79b4c47caa99` |
| host FS implementation | `31990f86302a1530c125ad85d2d053b997bd2450f0c9b0eb0e269b5b2699e040` |
| final independent profile report | `cb9af22eaabdcf9e43fa9b644eb74f49384d37390a13836b4911650e790f68d1` |
| blocked-status CSV | `f1fdd11b8a1625b7f6d5360132f0f16d0dc9939d2fa5ac95e8dafff16c864a44` |
| verified diagnostic CSV | `6f74f16918198945e13d7b86d544284224da004dd84fb8cfe7f66de056445d09` |

Validation actually run: new frozen release build, n=1 five-condition preflight, three rejected formal attempts, independent n=3 profile, 26 benchmark/publication tests, diagnostic/source-version/failed-cohort publication guards and scoped failed-connection cleanup. No crate regression suite or final formal performance PASS is claimed.

To obtain final-source performance results, editor auto-Cargo checks and other agents' build/test activity must remain disabled for the entire roughly five-minute timing window, not merely a quiet startup interval. Rerun into a new output directory; do not reuse or combine these rejected batches. Scope remains Linux HOST API, RAM-backed single-host noatime, without pvisor-run/VM/review guarantees.
