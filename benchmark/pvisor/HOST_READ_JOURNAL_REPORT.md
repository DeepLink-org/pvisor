# Read responsiveness during copy-up and journal fingerprinting

## Conclusions

**Independent read admission removes copy-up head-of-line blocking in this macOS FSKit workload; releasing journal locks removes unrelated read-observation waits during native fingerprinting.** No-journal and legacy read-only OPEN/READ median latency falls by 99.60–99.93%, and directory enumeration by 98.72–99.13%. Compact-journal copy-time baselines separate into 25 ordinary and five slow samples; both clusters remain visible rather than receiving one median ranking. All candidate read probes complete during copying in all 30 formal samples per condition, versus none on the baseline.

The legacy journal's first unrelated observation falls from 462.276 to 0.184 ms; its 64-path batch falls from 473.262 to 12.011 ms. Compact's batch falls from 453.158 to 2.385 ms. Both candidate journal batches finish while the large hash is active in 30/30 samples; baseline batches do so in 0/30.

**The tradeoff is measurable: legacy hash completion grows by 7.329 ms, or 1.59% (95% CI +0.62 to +1.96%).** None of the measured idle contrasts detects a difference; their confidence intervals do not prove equal overhead. Copy completion/tails and complete Agent tasks are separate questions.

## Motivation

A faster stat does not establish that opening a file, reading an existing descriptor or enumerating a directory remains responsive during a large copy. Shared journal locks can also serialize independent observations while hashing unrelated contents. The engineering decision is whether these two synchronization changes remove those waits without imposing unacceptable idle or long-task costs. These are B-FS-ENG results, not replacement B-FS-TOOLS user numbers.

## Experiment design

Two new offline release workers freeze the same source tree and dependencies. Baseline restores only five declared read-dispatch/journal files from `f109dd25f9b22165a4330fea489bd51e59996661`; candidate product source is `09bd1d59a781d3e279712d7fa0cb749a5f2614ea`. Other product changes, including stage sealing and VM dispatch, are identical. Compiler is Rust 1.98.0, aarch64-apple-darwin. Host is Apple M4, 24 GiB RAM, Darwin 27.0.0, APFS, macFUSE 5.4.0/FSKit. CPU affinity and whole-host resource limits are not enforced.

Each independent mode has three excluded warmup rounds and 30 seeded (`4207`) paired formal rounds, with condition order and A/B order shuffled. Slow valid samples are retained. Build/test interference snapshots run before, every 250 ms during, and after every trial. Any detected interference, missed window or correctness error fails the cohort. Snapshots cannot prove absence of arbitrarily short/inaccessible activity. There is no profiling during timing.

- **Read mode:** fresh real mounts, separate no-journal, legacy and compact strict-stage conditions. An allocated, non-sparse deterministic 1 GiB file is warmed and copied into a fresh upper. Only after observing a genuinely partial copy-up temporary are three probes released simultaneously: open/read/close an eight-byte file, pread 64 KiB through an unread held descriptor, and enumerate a fresh 64-entry directory. Each probe independently checks that copying is still partial when it begins. Concurrency prevents the first blocking probe from turning subsequent probes into idle measurements. Corresponding idle probes use independent paths. Writable-open latency is retained separately.
- **Journal mode:** one shared native Core, separate legacy/compact conditions, no FUSE and no artificial delay. Public `observe_read` fingerprints a warmed allocated 1 GiB file while observing 64 other paths. Resetting only the owned source's atime allows its first actual read to be detected; capture must still be active when probing begins. Read observations retain their normal non-durable policy, followed by untimed `sync_preimages`. All 129 retained fingerprints are verified outside timing.

Full source/upper SHA-256, source identity, expected metadata/held-file bytes, hardlinks, descriptor rebinding, rename, unused-copy cleanup, journal observations and normal unmount are correctness gates. Successful disposable uppers are removed only after checks; raw journals, logs, results, fixtures, source and binary receipts remain retained. Preflight outputs are separate and contribute no formal observations.

Median contrasts use 5,000 paired-round bootstrap resamples, seed 4207. CSVs retain absolute median-difference intervals as well as percentage intervals. The registry's descriptive split rule is applied before reporting: separated rows show cluster medians/counts and have no single median-change ranking. Reference P95/min/max are retained in CSV; 30 samples do not establish tail reliability or P99.

## Data and analysis

All tables are milliseconds, 30 samples per arm per condition. Separate read and native journal cohorts are never pooled.

### Real mounted reads during active copying

| Condition | Operation | Baseline ms | Candidate ms | Median change [95% CI] |
|---|---|---:|---:|---|
| nojournal | Read-only OPEN | 784.602 | 1.577 | -99.799% [-99.817, -99.782] |
| nojournal | Held-fd READ, 64 KiB | 784.597 | 0.521 | -99.934% [-99.936, -99.927] |
| nojournal | OPENDIR + 64-entry enumeration | 791.558 | 6.919 | -99.126% [-99.190, -99.074] |
| legacy | Read-only OPEN | 797.214 | 3.172 | -99.602% [-99.612, -99.579] |
| legacy | Held-fd READ, 64 KiB | 797.424 | 0.960 | -99.880% [-99.909, -99.860] |
| legacy | OPENDIR + 64-entry enumeration | 804.033 | 10.292 | -98.720% [-98.758, -98.695] |
| compact | Read-only OPEN | 800.275 (25/30); 2340.599 (5/30) | 2.219 | Separated distribution; no single ranking |
| compact | Held-fd READ, 64 KiB | 800.486 (25/30); 2340.717 (5/30) | 0.760 | Separated distribution; no single ranking |
| compact | OPENDIR + 64-entry enumeration | 807.137 (25/30); 2347.782 (5/30) | 9.737 | Separated distribution; no single ranking |

For compact rows, baseline cells list the ordinary 25/30 and slow 5/30 cluster medians. Across all three conditions and all three probes, candidate completion precedes copy completion in 30/30 formal samples, versus 0/30 for baseline. This supports eliminated queue waiting, not higher storage bandwidth. Full open/read/close candidate medians are 2.247 ms without journaling, 4.114 ms with legacy and 3.051 ms with compact; the CSV retains their complete baselines and intervals. Measurements include any parent xattr queries and read-only close callbacks issued by FSKit.

### Native observations during an active full-file hash

| Condition | Operation | Baseline ms | Candidate ms | Median change [95% CI] |
|---|---|---:|---:|---|
| legacy | First unrelated read observation | 462.276 | 0.184 | -99.960% [-99.964, -99.956] |
| legacy | 64 unrelated read observations | 473.262 | 12.011 | -97.462% [-97.521, -97.384] |
| compact | First unrelated read observation | 450.714 | 0.045 (25/30); 0.071 (5/30) | Separated distribution; no single ranking |
| compact | 64 unrelated read observations | 453.158 | 2.385 | -99.474% [-99.477, -99.464] |

The compact first-observation candidate separates into 25/30 at 0.045 ms and 5/30 at 0.071 ms. Its unsplit 64-path batch gives a complete-cohort median contrast: -99.47% [95% CI -99.48, -99.46]. All native journals retain exactly the expected 129 observations and their complete byte fingerprints. This is shared-Core lock-contention evidence; it does not measure the FUSE state lock or make durable-per-read guarantees.

### Idle behavior and long-task costs

| Condition | Operation | Baseline ms | Candidate ms | Median change [95% CI] |
|---|---|---:|---:|---|
| nojournal | Idle OPEN | 1.979 | 2.216 | +11.949% [-2.277, +20.598] |
| nojournal | Idle READ | 0.672 | 0.747 | +11.047% [-9.681, +23.156] |
| nojournal | Idle enumeration | 8.689 | 10.010 | +15.210% [-7.514, +26.719] |
| nojournal | Writable OPEN, 1 GiB | 788.513 | 789.595 | +0.137% [-1.780, +2.776] |
| legacy | Idle OPEN | 3.032 | 2.905 | -4.177% [-21.486, +10.282] |
| legacy | Idle READ | 0.709 | 0.674 | -4.946% [-15.833, +12.948] |
| legacy | Idle enumeration | 11.727 | 10.882 | -7.206% [-20.190, +13.147] |
| legacy | Writable OPEN, 1 GiB | 1267.015 | 1267.379 | +0.029% [-1.830, +0.959] |
| compact | Idle OPEN | 2.208 | 2.254 | +2.122% [-12.739, +18.718] |
| compact | Idle READ | 0.705 | 0.699 | -0.775% [-11.946, +16.194] |
| compact | Idle enumeration | 10.807 | 10.612 | -1.806% [-15.272, +12.827] |
| compact | Writable OPEN, 1 GiB | 1275.511 | 1251.440 | -1.887% [-3.126, -0.451] |
| legacy | Idle 64 observations | 11.558 | 11.575 | +0.151% [-1.564, +1.768] |
| legacy | Full 1 GiB fingerprint | 462.230 | 469.558 | +1.585% [+0.621, +1.964] |
| compact | Idle 64 observations | 2.486 | 2.505 | +0.737% [-1.507, +5.203] |
| compact | Full 1 GiB fingerprint | 450.739 | 452.237 | +0.332% [-0.398, +0.783] |

All idle intervals include zero. The no-journal point estimates are higher, but these samples do not detect a difference; absence of a detected regression is not an equivalence result. Legacy's hash median increases from 462.230 to 469.558 ms (+7.329 ms, absolute 95% CI +2.894 to +9.066 ms) while the 64 small observations overlap it. Compact hash time does not detect a difference. Concurrent small I/O/CPU work is a possible explanation for legacy's increase, not a proven attribution.

Compact writable-open median falls by 24.071 ms (-1.89%, absolute 95% CI -40.440 to -5.648 ms) in the read cohort; no-journal and legacy writable-open intervals include zero. This is total open/preparation completion under the concurrent probes, not isolated copy bandwidth. Tails remain uncertain: no-journal writable-open reference P95 rises from 952.6 to 3166.9 ms, with maxima 1469.6/4636.8 ms. These retained slow samples preclude a broad claim that long operations never regress, and n=30 cannot establish a stable tail change.

Normal read/write/open I/O, directory snapshot creation and publication can still occupy the shared filesystem state lock. Independent queues have no arrival-order guarantee for overlapping reads and mutations; fsync barriers and writable-handle lifecycle requests retain the mutation queue. Concurrent first observations may duplicate fingerprint work. Linux FUSE, VM/lazy images, cold disks, throughput and complete tool/Agent tasks are unmeasured.

## Retained evidence and reproduction

[Read statistics](host-read-summary.csv) and [journal-lock statistics](journal-lock-summary.csv) contain only processed results, with batch, configuration, sample counts, split distributions, reference tails and paired intervals. [Reproduction commands](README.md#read-dispatch-and-journal-fingerprint-locks-on-macos) create new independent outputs.

Local raw evidence (ignored, not public downloads):

- `.data/read-journal-build-20261010-2/`: frozen source/binaries/harness, compiler/build receipts and `derive.py` independent audit/export. The failed first build remains in `.data/read-journal-build-20261010/`.
- `.data/read-preflight-20261010/`, `.data/journal-preflight-20261010/`: ten preflight trials, never pooled.
- `.data/read-formal-20261010/`: 198 checked trials (180 formal, 18 warmup), 132 verified journal sets, zero failures and no slow-sample exclusions.
- `.data/journal-formal-20261010/`: 132 checked trials (120 formal, 12 warmup), 132 verified journal sets, zero failures and no slow-sample exclusions.

The independent audit rechecks all 330 trials, their result/launch files, full retained inputs, all 264 journal sets, frozen source/binary/harness hashes, schedule, guard results and regenerated statistics. Export leaves raw reports unchanged and suppresses whole-distribution rankings for separated cells. The earlier copy-up preparation report/CSV is an independent historical cohort and remains unchanged.

| Artifact | SHA-256 |
|---|---|
| Read raw report | `e572ec69552e15688b3307d0d745d62fab17cb602c61243d4c5f1dcc8d175ec0` |
| Journal raw report | `912a04d76e0dcf79f5d5800837ee913cebe4d8fc0fdffc7794fa6d555f34b7a0` |
| Read processed CSV | `93281296214b01a60cce6380d93c674dc3db3a3fe9631d90bcfc19378fef8476` |
| Journal processed CSV | `33d00305eafb9516ae31ed60e9d2a3816b38569818c8766a57998524cf1f03e6` |
| Baseline worker | `f5da8f9affbcfaa740a0639f05a11beaca6859dd391be03fa030b9297e6cfecd` |
| Candidate worker | `7cfc3d9b5870ec81768a80c5b80b9f340ef562cf7bc7ddcfb11f24c7bdfa7621` |

With retained inputs available, rerun the post-timing audit/export without changing the raw reports:

```sh
python3 benchmark/pvisor/.data/read-journal-build-20261010-2/derive.py
```
