# apply / drop: scale, conflicts and recovery

Applying 10 files takes about 15 ms, 1,000 about 0.84 seconds, and 100,000 about 5.5 minutes. Drop is much cheaper. Large apply is currently unsuitable for frequent low-latency commits.

## Motivation

Staging must eventually support safe submission: preserve concurrent host edits and recover interrupted commits, alongside acceptable performance.

## Experiment design {#interpretation}

Actual staged tasks overwrite existing text files. Lower content and upper count are verified before timing. Measurements cover only the apply/drop CLI, excluding stage generation and per-file validation. N=30/10/3 for 10/1,000/100,000 files. One warmup per action for smaller groups, none for 100,000. Three large stages are prepared concurrently; timed operations run sequentially. Conflict changes the first host file and requires refusal with all other targets unchanged.

## macOS

These workloads were measured on Linux; macFUSE/FSKit overhead and capacity remain unmeasured. Existing macOS/HVF results are retained in [VM startup](startup.md) and [VM memory](vm-memory/index.md), and are not substituted for this workload.

## Linux: 2026-10-04 {#results}

| Files | Operation | N | P50 / P95 / P99 ms |
|---|---|---|---|
| 10 | apply | 30 | 15.01 / 16.75 / 18.26 |
| 10 | drop | 30 | 3.38 / 3.82 / 4.72 |
| 10 | conflict | 30 | 4.07 / 9.44 / 11.67 |
| 10 | copy | 30 | 5.07 / 10.81 / 11.59 |
| 10 | git-apply | 30 | 0.73 / 0.85 / 0.88 |
| 1000 | apply | 10 | 836.38 / 1462.81 / 1864.62 |
| 1000 | drop | 10 | 24.44 / 26.02 / 26.24 |
| 1000 | conflict | 10 | 42.90 / 43.29 / 43.34 |
| 1000 | copy | 10 | 15.84 / 16.20 / 16.24 |
| 1000 | git-apply | 10 | 13.61 / 14.43 / 14.61 |
| 100000 | apply | 3 | 330396.22 / 337869.09 / 338533.34 |
| 100000 | drop | 3 | 996.55 / 1207.85 / 1226.63 |
| 100000 | conflict | 3 | 247496.21 / 253640.35 / 254186.49 |
| 100000 | copy | 3 | 1214.85 / 1359.59 / 1372.45 |
| 100000 | git-apply | 3 | 1464.63 / 1552.12 / 1559.90 |

### Analysis

Small batches fit interactive review; 1,000 files approach a second and 100,000 cost much more than copying or Git patches. The three largest applies took about 325–339 seconds; N=3 cannot establish stable tail latency. Conflict refusal at 100,000 files also takes about 247 seconds, revealing expensive validation. These costs are published without hiding the optimization gap.

Copy writes into an empty directory. Git apply updates equivalent text files but lacks the pVisor stage-cleanup/preimage/ledger/recovery protocol. They are cost controls with different semantics. Small and large batches retain separate provenance.

The 100,000-file stage emitted `trace append rejected: event exceeds size limit`. Target/ledger/conflict checks passed, but complete filesystem audit coverage is not claimed.

### SIGKILL recovery

After observing prepared / target_applied / committed, kill the apply process group, save the durable ledger at death and rerun apply. Verify all targets are new and the ledger is committed. Main 1,000-file prepared/target_applied injections passed three each; the committed window was missed because the process had exited normally. That injection did not pass. Follow-up uses 10,000 files. Full ledger parsing consumed the committed window; constant-size reads of the atomically published ledger tail then hit three committed SIGKILL injections and recovered. Both earlier misses remain archived and do not count as successful injections. State can advance between observation and death; the table reports actual durable state.

| Files | Requested kill state | N | Durable state at death | Recovery P50/P95/P99 ms |
|---|---|---|---|---|
| 1000 | prepared | 3 | prepared | 1587.60 / 1640.03 / 1644.70 |
| 1000 | target_applied | 3 | target_applied | 120.98 / 123.47 / 123.69 |
| 10000 | prepared | 3 | prepared | 8856.00 / 9023.85 / 9038.77 |
| 10000 | target_applied | 3 | target_applied | 354.28 / 371.49 / 373.02 |
| 10000 | committed | 3 | committed | 259.47 / 347.70 / 355.55 |

## Limits and next measurements {#acceptance}

SIGKILL does not test power loss, filesystem damage or lost disk writes. All file kinds/symlink/metadata combinations are not covered. Keep failures and before/after evidence when adding those cases. Large projects should bound each submitted batch; tiny-file results are not extrapolated to millions of files.

## Reproduction and evidence {#run}

Run from the repository root with a new output directory. This dynamic firmware entry requires the GNU/Linux CLI; static musl builds use a different firmware entry. This host has Linux, KVM/FUSE/user namespaces, Python 3.14, Rust/GCC, Git/rg, Node 24/npm and Podman/crun. The agent suite also needs the Claude/Codex CLIs.

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/gnu-linux/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output target/product-benchmark-new \
  --suites apply,baselines --samples 30 --warmups 3
```

Start with `--samples 1 --warmups 0` to check prerequisites. Workloads and correctness assertions live in `benchmark/pvisor/v1/`. Reports pin binaries, firmware and harness source with hashes. Failed operations never enter performance distributions. Effective sample counts are stated per page; P95/P99 from small samples describe this batch rather than production tail probabilities.

[Environment, artifacts and method](methodology.md#product-v1) · [Batch manifest](../../assets/benchmarks/product-v1-20261004/manifest.json) · [Per-sample CSV](../../assets/benchmarks/product-v1-20261004/samples.csv) · [Raw reports and diagnostic logs](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz). Reports retain dirty source status; executable SHA256 identifies the measured artifact. The archive excludes large rootfs/binaries and reproducible workspace payloads, while retaining input hashes and each batch's harness.
