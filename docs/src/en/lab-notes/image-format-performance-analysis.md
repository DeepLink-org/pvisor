# Image V2: engineering comparison of storage and reads

2026-10-25, B-IMAGE-FORMAT-ENG. V2 substantially shrinks this experiment's small-file inventories and improves open, repeated queries and enumeration. Cold random reads of high-entropy files still have a cost, and whole-process peak RSS does not uniformly fall. This is a same-host storage-backend A/B, not a Docker/OpenSandbox comparison, VM startup or real-application throughput result. See [Lazy Image V2](../design/lazy-image-v2.md) for current contracts.

## Motivation and experiment design {#method}

Compare the frozen V1 compressed local pool/filesystem backend with V2 to measure independent compressed directory indexes and small-file packing, including their costs. Both release test artifacts embed the same measurement logic (only rustfmt formatting differs; each batch verifies normalized source equality). The baseline pool already compresses with Zstd: this is not simply compression versus uncompressed storage.

| Cohort | Files/directories | Backend | Each file's 1 KiB payload |
| --- | --- | --- | --- |
| P10 | 10,000/1 | pool | Unique SHA-256 bytes repeated 32 times |
| P100 | 100,000/1 | pool | As above |
| D100 | 100,000/100 | pool | As above, round-robin directories |
| E10 | 10,000/1 | pool | Deterministic SHAKE256 high-entropy bytes |
| F10 | 10,000/1 | filesystem | Unique SHA-256 bytes repeated 32 times |

Each cohort shares one source tree with fixed timestamps and identical contents, with two new caches. Every file differs; extensive duplicate files do not inflate deduplication gains. Repeated-byte cohorts have strong within-file compressibility and do not represent real-image compression ratios. Initial storage publication excludes OCI downloads/layer extraction. Capacity includes format, pool marker, all metadata, upload receipts and data, excluding sources, artifacts and measurement evidence. Allocation uses `st_blocks × 512`, not exclusive device consumption, and excludes filesystem inode/directory-tree overhead.

Environment: AMD Ryzen 7 9700X, Linux 7.2.11-300.fc45 x86_64, rustc 1.98.0, Btrfs (existing host `compress=zstd:1`, configuration unchanged), fixed CPU 7. The host was not exclusive; manifests retain load averages and no slow samples were removed. Each cohort has 3 warmups and 30 formal pairs, randomizing version order with seed 437. Intervals bootstrap the median paired difference 5,000 times for a 95% interval; they are not intervals for median ratios.

Each round starts a new process with empty decoded caches and a retained host page cache, not cold disk. Sequence: Open → 1,000 random Stats → repeat the same Stats → 128 complete 1 KiB Reads → repeat the same Reads → full-tree List. C means first access within that stage; R means immediate repetition, without guaranteeing all accesses hit cache. Cold Read follows metadata Stats; List is not an independently cold scan. Attribute type/length, complete read bytes and enumeration counts are checked; any failure invalidates the batch. Process creation, preloading oracle inputs and semspec scheduling are outside stage clocks.

This replaces the whole backend. Node LRU residency stays capped at 16 MiB while its entry limit changes from 256 to 4,096; the content cache keeps its 64 MiB ceiling while admitting more small frames. Allocation is measured once at load and maintained with constant-time byte accounting. All gains cannot be attributed to XXH3 or any single encoding change.

## Inventories and latency {#results}

Values below are V1 → V2, MiB = 2²⁰ bytes. Metadata includes all controls/indexes/inventories/upload receipts, not only index.bin; allocated combines metadata and data.

| Cohort | metadata MiB | total logical MiB | total allocated MiB | Allocated reduction | data objects |
| --- | --- | --- | --- | --- | --- |
| P10 | 6.005 → 0.649 | 6.616 → 1.050 | 45.102 → 1.113 | 97.5% | 10,000 → 10 |
| P100 | 55.279 → 6.571 | 61.383 → 10.579 | 445.938 → 10.809 | 97.6% | 100,000 → 98 |
| D100 | 55.279 → 4.363 | 61.383 → 8.381 | 445.938 → 8.703 | 98.0% | 100,000 → 100 |
| E10 | 6.005 → 0.645 | 15.895 → 10.476 | 45.102 → 10.531 | 76.6% | 10,000 → 10 |
| F10 | 6.005 → 0.649 | 15.771 → 1.049 | 45.098 → 1.109 | 97.5% | 10,000 → 10 |


![Index and file-allocation comparison](../assets/benchmarks/image-format-capacity.svg)

Latencies are medians for each complete stage. Differences are paired V2−V1, negative indicating improvement. The last column compares the two version medians; negative percentages indicate regression.

| Cohort | Stage | V1 ms | V2 ms | Paired difference [95% CI] ms | Time reduction |
| --- | --- | --- | --- | --- | --- |
| P10 | Open | 0.525 | 0.136 | -0.388 [-0.401, -0.377] | +74.0% |
| P10 | Stat C | 6.779 | 6.553 | -0.228 [-0.266, -0.131] | +3.3% |
| P10 | Stat R | 1.338 | 0.821 | -0.517 [-0.529, -0.508] | +38.7% |
| P10 | Read C | 2.620 | 1.729 | -0.888 [-0.944, -0.850] | +34.0% |
| P10 | Read R | 1.159 | 0.364 | -0.799 [-0.804, -0.793] | +68.6% |
| P10 | List | 8.698 | 6.358 | -2.367 [-2.392, -2.279] | +26.9% |
| P100 | Open | 0.569 | 0.190 | -0.378 [-0.387, -0.365] | +66.7% |
| P100 | Stat C | 102.807 | 63.297 | -39.478 [-39.767, -38.943] | +38.4% |
| P100 | Stat R | 96.130 | 56.218 | -39.804 [-40.604, -39.229] | +41.5% |
| P100 | Read C | 25.371 | 9.724 | -15.628 [-15.736, -15.410] | +61.7% |
| P100 | Read R | 23.852 | 0.400 | -23.441 [-23.509, -23.370] | +98.3% |
| P100 | List | 135.320 | 92.275 | -43.013 [-43.649, -42.538] | +31.8% |
| D100 | Open | 0.607 | 0.185 | -0.421 [-0.430, -0.411] | +69.6% |
| D100 | Stat C | 117.911 | 22.207 | -95.720 [-96.168, -95.536] | +81.2% |
| D100 | Stat R | 111.072 | 1.401 | -109.683 [-110.066, -109.267] | +98.7% |
| D100 | Read C | 29.103 | 2.833 | -26.179 [-26.283, -26.020] | +90.3% |
| D100 | Read R | 27.188 | 0.377 | -26.800 [-26.864, -26.739] | +98.6% |
| D100 | List | 140.178 | 99.080 | -40.367 [-41.130, -40.025] | +29.3% |
| E10 | Open | 0.519 | 0.137 | -0.389 [-0.395, -0.369] | +73.5% |
| E10 | Stat C | 6.683 | 6.534 | -0.168 [-0.221, -0.119] | +2.2% |
| E10 | Stat R | 1.337 | 0.810 | -0.526 [-0.531, -0.513] | +39.4% |
| E10 | Read C | 2.774 | 4.036 | +1.316 [+1.253, +1.356] | -45.5% |
| E10 | Read R | 1.295 | 0.358 | -0.937 [-0.949, -0.924] | +72.4% |
| E10 | List | 8.574 | 6.311 | -2.255 [-2.315, -2.201] | +26.4% |
| F10 | Open | 0.537 | 0.148 | -0.394 [-0.400, -0.381] | +72.4% |
| F10 | Stat C | 6.635 | 6.281 | -0.397 [-0.431, -0.359] | +5.3% |
| F10 | Stat R | 1.337 | 0.814 | -0.526 [-0.534, -0.520] | +39.1% |
| F10 | Read C | 2.531 | 1.663 | -0.854 [-0.895, -0.819] | +34.3% |
| F10 | Read R | 1.141 | 0.362 | -0.781 [-0.788, -0.775] | +68.2% |
| F10 | List | 8.626 | 6.284 | -2.320 [-2.373, -2.282] | +27.2% |


The plot shows V1/V2; values above 1 mean V2 is faster.

![Relative speedup for each stage](../assets/benchmarks/image-format-latency.svg)

## Publication, memory and parameter corrections {#tradeoffs}

Publication has one observation and no interval; it is not steady-state statistics. RSS is the median of 30 process VmHWM values read after the entire sequence, including the test harness, oracle inputs, index/content caches and allocator. It is neither instance memory nor Open-only residency and establishes no guest mmap or application-memory benefit.

| Cohort | Publish s (single) | Process peak RSS MiB |
| --- | --- | --- |
| P10 | 2.136 → 0.494 | 20.45 → 22.54 |
| P100 | 23.044 → 5.092 | 31.99 → 37.11 |
| D100 | 23.455 → 5.562 | 33.07 → 37.07 |
| E10 | 2.271 → 0.552 | 20.10 → 22.58 |
| F10 | 2.007 → 0.469 | 20.24 → 22.42 |


The first candidate used approximately 128 KiB data frames and a 256-node cache. For 10,000 files, repeated Stat took 3.485 ms (baseline 1.340 ms). Small frames hit the entry limit too early, and misses recomputed allocation for every resident node. Raising admission to 4,096 entries under the original byte budget and maintaining constant-time accounting reduced repeated Stat to 0.810 ms. Those separate batches remain at `format-v2-pool-10000-1` and `format-v2-final-pool-10000-1`; they are not merged into final samples.

With large data frames, high-entropy E10 cold Read took 11.713 ms (baseline 2.774 ms). The final payload target is 16 KiB, retaining an individual file ≤64 KiB intact and validating a separate hard frame limit. Boundary tests still cover 1, 65,535, 65,536 and 65,537 bytes, random ranges and reads past EOF. Final E10 values appear above: amplification is reduced substantially but remains. Smaller frames add layout/compression overhead and may increase sequential remote request counts; local results cannot establish faster S3 access.

Disk savings primarily reflect compressed indexes, fewer tiny objects and less allocation slack. High entropy separates packing benefits from within-file compression. Separate regressions verify old-pack reuse, unchanged data bytes after rename/moves/chmod, source-independent reads, hardlinks, non-UTF-8 names, corruption refusal, publication CAS and cache budgets; these tests do not generate capacity percentages.

## Validation and remaining coverage {#validation}

Final validation:

- `mise run test -- pvisor pvisor-cli pvisor-daemon -- --no-fail-fast`: 1,053 passed; 30 explicitly ignored tests, including the measurement runner, are excluded from passes.
- Real Linux/KVM: filesystem and local SigV4 S3 fixtures pass guest-content and credential-isolation checks after publisher-source deletion. Additional checks read all 100 packed small files and mmap a packed file and a file spanning multiple 1 MiB chunks, retaining correct mappings after fd closure. This validates correctness, not mmap performance.
- Three native container/build acceptances pass: persistent upper, streams/terminal, failed/timed-out multi-step builds and VM cleanup after frontend death.
- Complete Podman-derived profile: 35 Bats + 83 Go pass without skips, using a frozen static binary, private stores, Bats 1.11.0, Go 1.27.1 and the pinned Alpine manifest below.
- Python: 516 tests, 498 passed and 18 skipped under existing conditions. An initial run concurrent with firmware compilation hit a timeout-cleanup timing failure; failure evidence is retained, and the full rerun passed without changing that assertion.
- Warnings-denied Clippy, Rust/Python formatting and static checks, bilingual documentation builds/link checks pass. Semspec lint validates 30 cases and 2 preparation documents, without ledger/snapshot edits or approve calls.
- Script regressions fail a nested configuration's space-containing target-dir routing assertion under host mise 2026.8.5. A complete rerun with mise 2026.10.19 in `/tmp` passes without changing system tools, firmware routing or test assertions.

Static acceptance binary SHA-256: `51622bc0f32900e67b6aeceb9666899ce0803c97ccdb35fa63b2fa9d7c60d089`. Alpine source: `mirror.gcr.io/library/alpine@sha256:c64c687cbea9300178b30c95835354e34c4e4febc4badfe27102879de0483b5e`. Acceptance artifacts, commands, per-case TAP/Go JSON and check logs remain under `benchmark/storage/.data/format-v2-16k-analysis/`.


Next measure real Python imports, Node.js, builds/tests, S3 requests/downloaded bytes, sequential/random amplification, concurrent VMs, copy-up and mmap. Those performance results are not available here; no first/steady VM creation speedup is claimed. Publication still plans metadata in O(N) memory, one index.bin is capped at 64 MiB, and disk quotas/online GC are not implemented.

## Reproduction and evidence {#evidence}

`benchmark/storage/image_format.py` reuses semspec repeat, command evidence and persistence, registered at `benchmark/REGISTRY.md#b-image-format-eng`. First attach the same ignored `portable/measurement.rs` runner to the baseline revision and candidate, build release test executables and freeze them. Ordinary unit-test process duration is not a substitute for the internal stage clocks.

```sh
python3 benchmark/storage/image_format.py \
  --baseline FROZEN_V1_TEST_BINARY --candidate FROZEN_V2_TEST_BINARY \
  --baseline-runner ORIGINAL_V1_MEASUREMENT_RS \
  --output benchmark/storage/.data/NEW_BATCH \
  --files 100000 --directories 100 --backend pool --runs 30 --warmup 3 --cpu 7
```

Final artifacts, harness/runtime, plans, command logs, per-object SHA-256/logical/allocation inventories, 30 pairs and summaries remain under `benchmark/storage/.data/format-v2-16k-*`, outside the published site. Initial diagnostics and 128 KiB candidate batches retain their original names and historical data.

| Identity | SHA-256 / revision |
| --- | --- |
| Baseline source | `05eda815da818815c5c7507d1017688ff0c7d241` + ignored runner |
| Baseline binary | `f26e5c0c153578220bfb5b151e77083dd22b8068ce91e3f7b3720599be5727fa` |
| Candidate binary | `9db67593ed0c023aab8fee3a1aa453e8dfb8876c2730e072efc0b0810287e3f9` |
| Runtime | `c67093d6eb672a1464c83efdfce1c4fbaac8f346837413a291849be63dbbd337` |
| P10 summary | `4e55658d5af78477ac45fed81b757e51649a51e6a6808dbfaa419cf73c176cbe` |
| P100 summary | `b41e67e22d0c8570ff34ed92f7a97b0e2db3cc6305e7a804e8a4fae8ca87a60f` |
| D100 summary | `da8e56af60a5dc7e38fad0f6fcda66d3fd676ad40a9ac639be03ea5d77f17d40` |
| E10 summary | `54a132485539e4a9b2525b8d94ae35d9da328273461c4c26bb2388cba70d0f47` |
| F10 summary | `a93e0eb8344369f9995ad0ab5f9d7ed3561ff7ae69a24a62a1cd282b84029123` |

