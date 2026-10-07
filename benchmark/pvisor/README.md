# pVisor benchmark runners

Read [the benchmark registry and writing rules](../README.md) before measuring or publishing. User questions, controls and roles belong to that registry; this manual owns commands and retention. Entry scripts carry `Benchmark:` declarations. Preparation, publication and plotting helpers serve their caller’s ID.

## Lazy image client startup

`B-LAZY-STARTUP` compares a Distribution registry's complete-image Docker path with a real `pvisor-cache serve` lazy VM path for the same pinned linux/amd64 manifest and workload. The default `ubuntu-shell` cohort uses Ubuntu 26.04; `numpy-script` is a separate Python/NumPy cohort. `torch-import` remains optional but has no formal samples; its failed attempts are retained, not performance evidence. Never pool different workloads or their preflight and formal samples. This is not OpenSandbox `pvisor-daemon serve`. See [the retained Ubuntu local report](LAZY_STARTUP_REPORT.md) and [derived Ubuntu CSV](lazy-startup-summary.csv); they do not establish NumPy or torch performance. The [bilingual user article](../../docs/src/zh/benchmarks/lazy-image-startup.md#numpy) includes the independent NumPy cohort, with [derived statistics](../../docs/src/zh/benchmarks/lazy-numpy-summary.csv), [differences/preparation](../../docs/src/zh/benchmarks/lazy-numpy-details.csv) and [provenance](../../docs/src/zh/benchmarks/lazy-numpy-provenance.csv). Its retained evidence is `benchmark/pvisor/.data/lazy-numpy-local-20261007/`; `derive.py` audits all 136 launches and regenerates the paired NumPy CSVs without changing the immutable report.

Finish other builds/tests before sampling. Docker access uses `sg docker -c`; KVM and both frozen executables must be usable. The successful NumPy preflight uses the existing `target/release/pvisor` launcher with the separately built `target/lazy-torch-build/release/pvisor-cache`, selected via `--cache-binary`. This static cache build includes Docker gzip compatibility; the directory name is historical and does not mean torch was measured. The commands below keep the default launcher directory `target/release` and override only the cache executable, not `--binary-dir`. A new launcher conflicts with the existing persistent Host listener: **do not kill or replace the user's listener** to run this benchmark. Preserve the compatible existing launcher/listener pairing. Finish artifact preparation and freeze the binaries before preflight; do not rebuild during sampling. Install `skopeo` and `openssl`; loopback ports 15000/15443/15444/15445 must be free. First pull the report's pinned Distribution image outside timing:

```sh
sg docker -c 'docker pull registry@sha256:ddf754342cfc8acc51a56d5d0ab6af06826461864460636d8bd5c546dab2a7b8'
python3 benchmark/pvisor/lazy_startup.py \
  --cache-binary target/lazy-torch-build/release/pvisor-cache \
  --output benchmark/pvisor/.data/lazy-startup-preflight-new \
  --samples 1 --warmups 0
# Only after preflight succeeds; always choose a new output directory.
python3 benchmark/pvisor/lazy_startup.py \
  --cache-binary target/lazy-torch-build/release/pvisor-cache \
  --output benchmark/pvisor/.data/lazy-startup-formal-new \
  --samples 30 --warmups 3
python3 benchmark/pvisor/lazy_startup.py \
  --analyze benchmark/pvisor/.data/lazy-startup-formal-new/report.json
just test-benchmark -q benchmark/pvisor/test_lazy_startup.py
```

Run the NumPy preflight and formal cohort separately, after other builds/tests have finished. The workload selects its own pinned source by default; do not reuse Ubuntu or failed torch output directories:

```sh
python3 benchmark/pvisor/lazy_startup.py \
  --workload numpy-script \
  --cache-binary target/lazy-torch-build/release/pvisor-cache \
  --output benchmark/pvisor/.data/lazy-startup-numpy-preflight-new \
  --samples 1 --warmups 0
# Only after NumPy preflight succeeds; always choose a new output directory.
python3 benchmark/pvisor/lazy_startup.py \
  --workload numpy-script \
  --cache-binary target/lazy-torch-build/release/pvisor-cache \
  --output benchmark/pvisor/.data/lazy-startup-numpy-formal-new \
  --samples 30 --warmups 3
python3 benchmark/pvisor/lazy_startup.py \
  --analyze benchmark/pvisor/.data/lazy-startup-numpy-formal-new/report.json
```

The NumPy source is the pinned linux/amd64 slim manifest `docker.io/amancevice/pandas@sha256:9a3a94039175ac799ad33c1a207997994ff9b24814e06508dddfa259b1ed9159`, with Python 3.13.14 and NumPy 2.5.2. Despite the image name, **pandas is not imported**. `/usr/local/bin/python -B -u` disables bytecode writes and buffers no stdout; explicit `OMP_NUM_THREADS`, `MKL_NUM_THREADS` and `OPENBLAS_NUM_THREADS` are 1, with `PYTHONHASHSEED=0`. Before its unique ready marker, the workload verifies exact Python/NumPy versions, constructs int64 `arange(16).reshape(4, 4)`, checks square sum 1240 and `(x @ x.T).sum()` 3680. Ready is the output after all checks; completion includes successful process exit. This is a small numerical script, not a pandas workload, training test or large computation.

Optional `torch-import` remains supported with `docker.io/determinedai/pytorch-cpu@sha256:875cbd3391016a74c42cfb0b3712d3b70f5b803a04b80eebd7f2a46b9d53d18d`, a confirmed CPU-only older 2024 image with Python 3.10.14 and PyTorch 2.0.1+cpu, not latest torch. It validates versions, absence of CUDA and a single-thread CPU tensor operation before ready. **No formal torch samples are available.** Failed torch reports, logs and caches remain in `.data/`; failures cannot be analyzed as successful samples or pooled with NumPy. Any future optional torch measurement needs its own successful preflight and new independent output directories.

Plan disk space before either NumPy invocation: the compressed slim image is approximately **115,924,658 bytes (116 MB)**, not a bound on total retention. Optional torch's compressed image is approximately 1 GB and retained failed torch caches may still contain imported libtorch files. Each new NumPy output directory retains its own preparation cache, prepared service image store and per-pair client caches containing imported Python/NumPy files; unpacked images, warmup caches, logs and provenance add further space. Preflight and formal directories are separate retained copies. Temporary registry storage and Docker image storage also require disk headroom during the run; normal cleanup removes the private registry container/volumes and benchmark image reference, not the retained output caches. Allow substantial additional disk headroom for these caches and Docker storage rather than budgeting only the compressed download. Keep reports, raw logs, image manifest, preparation timings, frozen harness and source/binary hashes in their ignored `.data/` directories; do not prune evidence during sampling. `blob_bytes` counts unique config/layer digests, including config and counting repeated layer descriptors only once, while `compressed_layer_bytes` sums layer descriptors.

The default Ubuntu source digest freezes the observed `ubuntu:latest`, rather than resolving a moving tag each trial. Default firmware is embedded in the static musl CLI; `--firmware` is only for dynamic GNU builds. The runner starts/cleans its private registry container, bounds service/process lifetime and stores requests, logs, harness and source/binary hashes in the new ignored directory. It deletes only its benchmark workload image reference, never prunes Docker. Upstream image copying/cache preparation requires network access and can be substantial; it is retained separately from client times. Cache preparation imports the same manifest from Docker Hub, since the OCI reader requires trusted HTTPS. Registry client traffic uses HTTPS through a counting proxy, cache uses authenticated plain TCP; application response accounting excludes full transport overhead. No injected latency/bandwidth or true WAN measurement is supplied.

A cold Docker sample must fetch every blob; a cold lazy sample must read content into a fresh client cache. Warm Docker must make no registry requests; warm lazy can make metadata requests but must fetch no file content. Correct output, successful exit and the VM Run Bundle are checked; failures invalidate the campaign, and slow valid samples are not discarded. `--warmups 3` also performs one initial excluded round. Container 2 GiB limits and VM 2 GiB guest RAM are not equal enclosing-memory controls; Docker daemon/containerd and proxy CPU work is not fully pinned. Build-time source relationships of preexisting artifacts remain unverified. Do not infer WAN, pure lazy-algorithm, whole-system equal-budget or checkout optimization claims.

The immutable measurement report is kept as generated; `--analyze` writes separate `analysis.json` using the existing publication cluster rule and paired bootstrap. The original cohort's source/binary hashes and frozen harness identify its actual measurement implementation; later statistical fixes do not alter samples or replace the original harness.

## vCPU observation M0

`B-VCPU-IDLE-ENG` / EXP-001 M0 的 [实验计划](vcpu_idle_plan.md)定义真实 guest sleep/busy/短 timer、1/2 CPU 与 SMP 单 CPU busy 负对照，以及 observer off/on seeded 随机配对。真实 SDK example 直接在 ready callback 接通 `VmmHandle` 的 `VcpuObservationControl`；不走产品 src 修改，不 pause/offload。KVM_RUN 内 Unknown、HVF WaitingForEvent 原样保留；卸载收益未测，M1/M2 未实现。

先在仓库根构建并冻结（不启动 VM），输出必须为 NEW；GNU debug build 可用于可运行性预检，正式性能比较必须使用同批同制品，不能把 debug 数字当 release 成本。

```sh
python3 -m unittest discover -s benchmark/pvisor -p test_vcpu_idle.py -v
CARGO_BUILD_JOBS=4 python3 benchmark/pvisor/vcpu_idle.py --build \
  --output benchmark/pvisor/.data/vcpu-build-new
benchmark/pvisor/.data/vcpu-build-new/vm_vcpu_observe --describe
```

用户选择资产后，才运行下列独立预检；这里 `/absolute/path/...` 是必须替换的输入占位说明，不是仓库提供或已验证的路径。

```sh
python3 benchmark/pvisor/vcpu_idle.py \
  --build-receipt benchmark/pvisor/.data/vcpu-build-new/build-receipt.json \
  --rootfs /absolute/path/to/prepared/rootfs \
  --firmware /absolute/path/to/firmware-directory \
  --init /absolute/path/to/static/pvisor-guest \
  --python /usr/bin/python3 \
  --output benchmark/pvisor/.data/vcpu-preflight-new --pairs 1
```

正式批换新的输出目录并使用 `--pairs 5`（或预先选择更多 pair）。默认每个 worker 3 秒、10 ms 采样、90 秒进程组 timeout，最多一台 VM。rootfs 必须有同架构 Python 3/hashlib/multiprocessing/sched affinity；init 必须是理解 `/.pvisor-guest.json` 的静态 Linux pvisor-guest，不能拿任意 `/sbin/init` 替代。firmware 使用 API 的 `firmware_name`，embedded kernel 则记录实际嵌入 hash；不自动找本地路径或下载。Linux 需要 `/dev/kvm` 授权，macOS build helper 自动签署 HVF entitlement（平台仍需实机验证）。

每个试次复制 rootfs，保存完整日志、guest digest/affinity/重叠时间校验、有界 samples、初始 snapshot 与失败；保留 source/binary/compiler/build 和 input receipts。`report.json` 包含完整性、失败数量、窗口/Unknown/拒绝原因和配对 wall/CPU bootstrap CI。任何失败/来源变化最终 exit 非零；不剔除慢有效样本。观察者成本包含采集、采样和 JSONL I/O，VM wall 含启动退出，不是纯 collector 成本、全机 CPU 或卸载收益。不要与构建/测试/其他实验并行，不改 global sysctl。真实 Linux/KVM guest 的独立预检与 5-pair 工程 A/B 结果见 [M0 实测报告](vcpu_idle_report.md)，含完整命令、来源摘要、失败保留与解释边界；HVF 仍未实机验证。

## Memory mechanism diagnostic

`B-MEMORY-DIAG` uses the Linux-only ignored `ram_dedup::tests::memory_diagnostic` test in `pvisor-vm`. It checks same-inode private baseline sharing, COW isolation and reference lifetime, optional KSM registration, and raw disk-backed reclaim without KVM/FUSE. It requires KSM `run=0` and never changes global settings. Use a fresh output directory and retain the log and source/binary receipts; registration is not measured merging, and mapping RSS/PSS is not whole-machine savings.

```sh
mkdir -p benchmark/pvisor/.data/memory-diagnostic-new
cargo test -p pvisor-vm --lib ram_dedup::tests::memory_diagnostic -- \
  --exact --ignored --nocapture --test-threads=1 --format terse \
  > benchmark/pvisor/.data/memory-diagnostic-new/raw.log 2>&1
```

The explicit `cargo test` invocation is the special diagnostic runner, not the default conventional validation command. [The retained mechanism report](MEMORY_DIAGNOSTIC_REPORT.md) contains the actual 2026-10-06 commands, settings, derived results and limitations. Its diagnostic results do not populate B-VM-MEMORY user-facing benchmark pages.

## Memory scale engineering protocol

[B-MEMORY-SCALE 的完整中文协议](MEMORY_SCALE_PLAN.md)覆盖同源恢复的 baseline/KSM advice A/B、COW 与 fresh-live raw/compressed 两次卸载恢复。每轮精确 54 格；预检一轮单独保存，通过后正式五轮（270 个 sequential batch）。实际最多四个 VM，生产者 reap 后才恢复，完整组固定四核/2 GiB/零 swap。共同 inode 与 advice 需审查 smaps/FD，登记不等于合并；独立 inode 对照当前不支持/未测。KSM 全局设置只读，扫描关闭和管理员预启用分别成 cohort；当前无连续监控，debug 计时不是生产延迟。

从仓库根运行以下命令；先按协议完成构建与来源冻结，确认本地 build receipt 匹配 example，且短磁盘输出目录为 NEW。完整矩阵使用脚本默认值；保留失败、日志与收据，不在 OOM 后提高预算、不用旧批补格。

```sh
python3 benchmark/pvisor/memory_scale.py \
  --build-receipt benchmark/pvisor/.data/memory-scale-build-20261006/build-receipt.json \
  --rootfs /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/density-env4/rootfs \
  --firmware /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/firmware \
  --output /home/reiase/workspace/pvisor/benchmark/.data/s4p --preflight

# 仅在完整预检和协议证据审查通过后运行；不要与预检并行。
python3 benchmark/pvisor/memory_scale.py \
  --build-receipt benchmark/pvisor/.data/memory-scale-build-20261006/build-receipt.json \
  --rootfs /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/density-env4/rootfs \
  --firmware /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/firmware \
  --output /home/reiase/workspace/pvisor/benchmark/.data/s4f --samples 5
```

协议定义了 inode/pin、全摘要和 mutable-state 门禁、恢复余量及生命周期 peak 的限制。后续父协调者报告留在本工程目录，不发布为用户 benchmark 结果；第二种 KSM cohort 必须另选新的短输出目录。

### Firecracker KSM control

`firecracker_ksm.py` serves B-MEMORY-SCALE engineering A/B. It boots at most four fresh Firecracker PCI VMs (256 MiB / 1 vCPU each), with a 64 MiB byte-validated payload using the pVisor page generator, a 20-second observation window, and 25/100% mutation plus peer/survivor verification. Its complete worker group uses four CPU quota / 2 GiB / zero swap. KSM configuration is read-only and must match the administrator-enabled `run=1`, `pages_to_scan=100`, `sleep_millisecs=20`. This is not a same-kernel/runtime/backing comparison or a production density measurement.

```sh
python3 -m unittest discover -s benchmark/pvisor -p test_firecracker_ksm.py -v
python3 benchmark/pvisor/firecracker_ksm.py \
  --assets /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/assets \
  --output /home/reiase/workspace/pvisor/benchmark/.data/f4k-new
```

Use a NEW output directory. The runner freezes sources, installed Firecracker/API schema, static guest worker and receipts; retain complete smaps, cgroup counters, logs and failures. Installed v1.13.1 has no supported guest-memory-merging option: advice-on is `unsupported`, so an otherwise successful advice-off preflight returns nonzero for the incomplete matrix. No ptrace/LD_PRELOAD workaround is substituted. See [the engineering comparison](FIRECRACKER_KSM_REPORT.md) for results and the distinction between scanner enabled and RAM mergeable.

## Cold runtime/storage validation

`B-COLD-STORAGE-DIAG` separates codec storage gains from live VM reclaim. Its retained cohort predates the Linux pager implementation; it measured codec storage only. `cold_storage_probe` uses a fresh instance-exclusive `CompressedPool` for 64 MiB fill, unique patterned or unique random data, 64 KiB blocks, two complete restores and full-byte/SHA-256 validation. This is not a local pager implementation. Encoded bytes exclude metadata, allocator, scratch and original RAM; no VM residency gain is inferred.

```sh
CARGO_BUILD_JOBS=4 cargo build --locked -p pvisor --example cold_storage_probe
# Freeze source/compiler/build receipts and binary before measurement.
# Run each pattern in three fresh sequential processes, retaining stdout/stderr.
target/debug/examples/cold_storage_probe --pattern fill --trial 1
target/debug/examples/cold_storage_probe --pattern patterned --trial 1
target/debug/examples/cold_storage_probe --pattern random --trial 1
```

Use a NEW ignored evidence directory, record prechosen order, affinity and complete failures, and do not build/test during sampling. The retained experiment used a frozen debug binary, seeded interleaving, CPU 0, and no cgroup cap; exact commands, source/binary receipts and all nine runs are retained under `.data/cold-storage-20261006/`. See [the historical storage verification report](COLD_RUNTIME_REPORT.md) for those observations; its pre-implementation support statements are historical. They do not substitute for live VM validation.

`B-COLD-RUNTIME-ENG` now validates the experimental Linux x86_64 kernel-fault userfaultfd pager with instance-local storage. Enable `[vm].cold_ram_compression = true` or `--vm-cold-ram-compression`; it is default-off and distinct from FUSE `ram_compression`. Userfaultfd authority must be granted by the administrator. Local compression rejects file/COW backing, KSM advice and snapshot/offload combinations; the external daemon pool is a separate store choice on the same Linux pager path. The policy uses eviction/refault probing, not full read-heat tracking.

```sh
python3 -m unittest discover -s benchmark/pvisor -p test_linux_cold_runtime.py -v
CARGO_BUILD_JOBS=4 cargo build --locked -p pvisor --example vm_cold_runtime
# Freeze source and binary receipts before measuring; use a NEW short output path.
python3 benchmark/pvisor/linux_cold_runtime.py \
  --example /absolute/path/to/frozen/vm_cold_runtime \
  --build-receipt /absolute/path/to/build-receipt.json \
  --rootfs /absolute/path/to/prepared/rootfs \
  --firmware /absolute/path/to/firmware \
  --output /absolute/path/to/new/short/output
```

The four-condition preflight uses one fresh 256 MiB/1-vCPU VM per cell, 64 MiB repeated/random payloads, cold off/on, fixed 20/35-second windows, four CPU quota/2 GiB/zero swap and two full recovery/mutation/device-I/O checks. Maximum simultaneous VM count is one. It requires 30 quiet seconds per launch and rejects detected VM/build interference. Trusted startup RAM intervals must match complete smaps VMA intervals before attributing isolated PSS. Preserve failed cohorts and all receipts; no production density or small-sample tail-latency claims. See [the Linux implementation and measured effects](LINUX_COLD_RUNTIME_REPORT.md).

### Linux compression reproduction evidence

[Linux memory compression benefits/costs and recorded reproduction commands](MEMORY_COMPRESSION_REPORT.md#recorded-commands-and-reproduction).

[Frozen-source GNU release preflight](COLD_RUNTIME_RELEASE_REPORT.md) retains a separate four-cell engineering cohort with independently regenerated full-payload digests, unchanged input inventories and post-run provenance checks. Its same-directory summary/phase CSVs contain processed observations only; raw trials, smaps, guards and build receipts remain in ignored `.data/`. Each cell has n=1, so these results do not supply user-facing latency, density or industry rankings.

## Data and publication

`publish_apply_concurrency.py --report benchmark/.data/concurrent-new/report.json --output docs/src/en/benchmarks` audits three planned B-APPLY mid-write probes separately from latency. It verifies retained build/harness receipts, each injection and outcome, final target contents, ledger and conflict stderr. Missing or duplicate trials are rejected; missed windows remain unknown, and silent overwrites remain failures. Run after the probe exits; publish the matching CSV beside both locale articles. This does not establish protection against every race between a final check and rename.

Apply sampling honors `--samples` and `--warmups` at every file count, including 100,000 files. Use explicit smaller values in a separate preflight output; file count never silently reduces formal rounds. Fresh stages and Git patches are prepared outside the application timer. Large sweeps can take hours; retain failures and partial reports instead of filling conditions from previous runs.

Keep raw reports, per-trial samples, stdout/stderr, failures, input hashes, binaries and frozen harnesses in `.data/`. The repository ignores that directory at every depth. A new output directory is required for every run; retain slow valid samples and failed preflights. Do not build, run other tests or sample unrelated workloads during measurement.

Public Markdown contains derived comparisons. Supporting CSVs sit beside each article in `docs/src/{zh,en}/benchmarks/`, with download links. They identify cohort, resource budget, sample count, statistic and source digest. Full typed TSV reports, sample CSVs and archives stay local. Existing evidence is preserved in `docs/src/assets/benchmarks/.data/`; the verified migration inventory is in `benchmark/.data/raw-migration.json`.

For large parked-capacity sweeps, `parked_density.py --archive-completed-snapshots` losslessly retains completed Job snapshot/restore trees as each batch's `snapshot-artifacts.tar.zst` and `snapshot-artifacts.json`. This happens after the measured service exits, outside all task timers, cgroup phase memory and CPU. Logs, configurations, full recovery proofs, Run Bundles and final upper changes remain directly readable. Every archive member's bytes, links, mode, numeric ownership, nanosecond timestamp and extended attributes are checked before original generated trees are removed. Failed, unknown or incomplete batches keep their complete artifacts. Archive compression is a retention operation, not a measured VM compression mechanism: zstd level 3 uses one thread and a maximum 512 MiB matching window in the outer coordinator, after the measured cgroup has exited. Restore into a new scratch directory with:

```bash
python3 benchmark/pvisor/retained_snapshot_archive.py \
  --restore benchmark/.data/parked-new/0/snapshot-artifacts.json \
  --destination benchmark/.data/parked-restored-new
```

`publication.py` publishes derived CSVs from a complete reference-runtime report. It rejects incorrect, duplicate and incomplete samples, keeps failures separate, never pools cohorts, omits P99 and omits P95 below 30 samples. P95 remains descriptive, not a stable tail-latency guarantee. A separated distribution replaces a single P50 with both cluster counts and medians: each cluster must contain at least `max(5, ceil(N*0.1))` samples, the largest eligible gap must be at least 20% of the overall median and more than three times the median adjacent gap, and cluster medians must differ by at least 1.5×. This is a descriptive rule, not a diagnosis.

```bash
python3 benchmark/pvisor/publication.py \
  --report benchmark/.data/reference-new/report.json \
  --output docs/src/zh/benchmarks
just test-benchmark
```

CSV summaries are not substitutes for raw evidence: a fresh checkout can read tables and inspect derived provenance, but rerunning requires prepared tools, firmware and local raw inputs. Do not create public links to ignored `.data/` paths. Site builds explicitly exclude them.

`publish_reference_campaign.py` combines derived tables from complete reference cohorts while retaining their individual batch/workload identities. It verifies retained binary/source receipts, refuses duplicate workload/backend cohorts and incomplete successful cells, and adds paired-round bootstrap confidence intervals. Separated distributions retain cluster statistics instead of receiving one median ranking.

```bash
python3 benchmark/pvisor/publish_reference_campaign.py \
  --reports benchmark/.data/ready-new/report.json \
            benchmark/.data/filesystem-new/report.json \
            benchmark/.data/tools-new-report/report.json \
  --output docs/src/en/benchmarks
```

## Same-host runtime comparison

B-STARTUP, B-FS-TOOLS and B-AGENT-TASK use `reference_baselines.py`. Native, pVisor host/staged/VM, rootless Docker, Firecracker PCI, QEMU q35 and QEMU microvm share offline tools and fixtures. The daemon and images are already prepared. Use two allowed host CPUs, 2 vCPU per VM, 128 MiB for the shell-ready probe and equal configured memory for all tool VMs. Use 16 GiB for Python/Node/Rust tools. Docker/native memory is not capped: this is a CPU-controlled comparison, not identical resource enforcement.

Prepare a private rootless Docker daemon on a short socket path. Supply its actual host PID so the runner checks ownership and pins only that daemon’s tree. Existing system Docker is not required or modified. Docker tasks also use the in-container affinity helper. Images, ext4 templates and pVisor firmware differ in execution semantics; the results are not pure VMM or security rankings.

```bash
bash benchmark/pvisor/prepare_reference_kernel.sh \
  /absolute/path/to/clean/linux-source benchmark/pvisor/.data/kernel-new
python3 benchmark/pvisor/prepare_reference_env.py \
  --output benchmark/.data/tools-new \
  --tools-rootfs /absolute/path/to/prepared-tools \
  --kernel-elf benchmark/pvisor/.data/kernel-new/vmlinux \
  --kernel-bzimage benchmark/pvisor/.data/kernel-new/arch/x86/boot/bzImage \
  --kernel-config benchmark/pvisor/.data/kernel-new/.config \
  --docker-host unix:///tmp/pvisor-benchmark-docker.sock
python3 benchmark/pvisor/reference_baselines.py \
  --assets benchmark/.data/tools-new \
  --binary /absolute/path/to/frozen-release/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --output benchmark/.data/reference-new \
  --docker-host unix:///tmp/pvisor-benchmark-docker.sock \
  --docker-root-pid 12345 --cpu-affinity 0,1 \
  --memory-mib 16384 --host-isolation rootless_process \
  --staged-isolation rootless_process \
  --modes filesystem --samples 60 --warmups 3 --seed 20261005
```

Run `--samples 1 --warmups 0` into a separate new directory first. Run `--modes ready`, `--modes filesystem` and `--modes tools` separately, each with its own output directory and benchmark ID. `env,tools,claude,codex` share B-AGENT-TASK and may be selected together. Every group requires valid outputs, declared isolation and complete staged writes; original host inputs must remain unchanged for staged jobs. `ready` measures first output, `filesystem` runs seven checked operations, and `tools` performs the fixed repair/test/diff plan. Each row records its benchmark ID. `claude,codex` are separate real-CLI workloads with deterministic local responses, not real inference or default nested-sandbox rankings. Failures make the runner exit nonzero after unaffected cases finish.

Tool preparation needs Linux x86_64, KVM, FUSE, user namespaces, a GNU pVisor CLI, firmware, Docker/Firecracker/QEMU, the Rust musl target, Git/rg/Python/Node/GCC and e2fsprogs. CLI modes also need their installed clients. Input copying, kernel builds, image import and downloads are outside timers. First output, result return and process exit are separate metrics. Record binary/source/harness digests, host kernel and tool identities with every run.

### Explicit Firecracker kernel controls

The question is **how does kernel source affect first correct output waiting under the same prepared userspace and budget?** `fc-system` is the primary official distribution stock-kernel control (the default FC backend in `ready` mode); `fc-reference` is an independent custom-kernel supplemental control. Explicit `--backends firecracker` remains compatible with old assets, but its kernel is labeled **legacy-reference/unknown**, never system/stock. Existing custom-kernel preparation above is not stock-kernel evidence. Other workloads retain their historical default backend list, payloads and normal success-exit requirements. Use explicit `--backends fc-system,fc-reference` for stock/custom controls in any workload; use an explicit legacy backend list when repeating a historical ready matrix.

`firecracker_kernels.py` freezes existing exact bytes, without downloading or rebuilding and without using pVisor firmware. For stock kernels supply the recorded official `/boot/vmlinuz-*`, matching `/boot/config-*`, a trusted Linux `scripts/extract-vmlinux`, and a preprepared initrd if stock drivers need one. The extractor runs with bash and its stdout is retained as the ELF kernel; original vmlinuz, extractor, config, source metadata and optional initrd are retained and SHA-256 gated. Custom reference kernels must be independent prebuilt ELF bytes with config and build provenance. Provenance is an operator assertion, not automatic signature/package authentication: independently verify official package origin and config matching before preparing. Review the extractor before executing it; it has the preparing user's authority. The helper never installs modules into, or changes, the common rootfs. An incompatible stock kernel/initrd fails preflight, not an excuse to rebuild a minimal kernel and label it stock.

Stock `--provenance` JSON requires nonempty `distribution`, `package`, `package_version`, `source_url`, `origin: "official-distro-stock"` and `pvisor_firmware: false`. Custom JSON requires nonempty `source_url`, `source_revision`, `build_command`, `compiler`, `origin: "independent-custom"` and `pvisor_firmware: false`. Retain additional package checksum/signature evidence in that JSON as appropriate. `--output` must be new.

```sh
python3 benchmark/pvisor/firecracker_kernels.py --kind fc-system \
  --vmlinuz /absolute/path/to/official/vmlinuz \
  --config /absolute/path/to/official/config \
  --extractor /absolute/path/to/trusted/extract-vmlinux \
  --provenance /absolute/path/to/stock-source.json \
  --output benchmark/.data/fc-system-new
# Add --initrd /absolute/path/to/exact/initrd only if needed.
python3 benchmark/pvisor/firecracker_kernels.py --kind fc-reference \
  --kernel /absolute/path/to/independent/vmlinux \
  --config /absolute/path/to/independent/config \
  --provenance /absolute/path/to/reference-build.json \
  --output benchmark/.data/fc-reference-new
python3 benchmark/pvisor/reference_baselines.py \
  --assets /absolute/path/to/common/prepared-assets \
  --binary /absolute/path/to/frozen/pvisor \
  --backends fc-system,fc-reference --modes ready \
  --fc-system-receipt benchmark/.data/fc-system-new/kernel-receipt.json \
  --fc-reference-receipt benchmark/.data/fc-reference-new/kernel-receipt.json \
  --fc-ready-policy ready-only --cpu-affinity 0,1 \
  --output benchmark/.data/fc-ready-preflight --samples 1 --warmups 0
```

Both variants use identical FC boot args, the same prepared ext4 template/fresh trial copy, two vCPUs, configured RAM, host affinity, Warning logger and termination procedure. Only kernel/required initrd and recorded provenance differ; configuration and initrd differences limit causal attribution to kernel source alone. Receipt and all artifact hashes are validated before/after each trial and the cohort, including the final gate after failures. The common prepared-input gate still applies. Declare the existing verified `--resource-budget` and matching budget options when enforcing a whole-parent budget; affinity alone is not a complete resource-enforcement claim.

For QEMU q35 and microvm add `--qemu-system-receipt` pointing to the same `fc-system` receipt. QEMU uses its retained original stock vmlinuz (Firecracker uses the extracted ELF), plus the same optional initrd. Both QEMU variants recheck all receipt artifacts before/after each trial and the cohort and record `kernel_variant: qemu-system`; without this option they remain `legacy-reference/unknown`.

Stock QEMU microvm keeps CMOS RTC enabled (`rtc=on`) because distribution kernels probe it during initialization; disabling it can add timeout waits. ACPI, option ROMs, PIT and PIC stay off. Legacy custom-kernel microvm keeps its historical `rtc=off` configuration. Changed device configurations require separate complete cohorts, never pooled timing samples.

Fedora stock kernels with built-in virtio PCI/block and ext4 but modular virtio MMIO can use this minimal common initrd. It compiles a static loader, retains the exact installed module and source/compiler/hash receipt, loads that module, mounts `/dev/vda`, and executes the prepared `/bench/init`. It leaves the host boot files and common userspace unchanged. This measures stock-kernel startup with minimal prepared userspace, not full systemd/SSH startup. Preparation is outside timing; use a fresh output and supply the generated image to `firecracker_kernels.py --initrd`.

```sh
python3 benchmark/pvisor/prepare_stock_initrd.py \
  --kernel-release 7.2.8-200.fc44.x86_64 \
  --output benchmark/.data/stock-initrd-new
# Prepare the stock receipt above with --initrd .../initrd.cpio.gz, then:
python3 benchmark/pvisor/reference_baselines.py \
  --assets /absolute/path/to/common/prepared-assets \
  --binary /absolute/path/to/frozen/pvisor \
  --build-receipt /absolute/path/to/build-receipt.json \
  --firmware /absolute/path/to/frozen/firmware \
  --backends pvisor-vm,fc-system,qemu,qemu-microvm --modes ready \
  --fc-system-receipt benchmark/.data/fc-system-new/kernel-receipt.json \
  --qemu-system-receipt benchmark/.data/fc-system-new/kernel-receipt.json \
  --cpu-affinity 0,1 --samples 1 --warmups 0 \
  --output benchmark/.data/stock-ready-preflight-new
```

Only after all selected backends pass, repeat into a new short output with at least 30 samples and three warmups. If normal FC shutdown is unsupported, keep that failed preflight and select the documented ready-only policy in a separate cohort; do not supply FC Completion numbers.

`--fc-ready-policy normal` is default and requires normal process exit zero plus checked guest output. `ready-only` is optional and rejected outside `--modes ready`: exactly one ordered Ready, successful matching Result and Exit0 must arrive before SIGTERM is sent to the owned FC process group. All final stdout/stderr are checked again; panic, duplicate/missing markers, timeout/SIGKILL and uncontrolled nonzero exit remain failures. Ready-only rows have `completion_ms: null` and no Completion summary. This proves checked output, not normal guest shutdown; non-FC backends retain normal completion even in an invocation with the FC policy. Keep policies in separate cohorts. `publication.py` accepts complete controlled ready-only rows and omits FC Completion; `publish_reference_campaign.py` additionally rechecks retained stock-kernel receipts, supplies readiness comparisons against explicit FC variants, and skips absent Completion comparisons.

Run only targeted conventional tests now; do not start preflights or formal sampling while parent builds/tests run:

```sh
python3 -m pytest -q benchmark/pvisor/test_reference_baselines.py benchmark/pvisor/test_firecracker_kernels.py
```

After parent build/test completion and capability/input review, run a separate preflight, then at least 30 seeded interleaved formal samples with three warmups in a new output. No measurements are supplied by these corrections. Formal stock/reference samples must not be merged with historical user numbers.

### Explicit whole-parent resource budget

The default invocation above controls CPUs and configured guest RAM. To enforce a shared total memory budget, first create a fresh owned user slice and private rootless Docker daemon; existing daemons must not be moved into it to claim their preexisting memory charges.

```bash
python3 benchmark/pvisor/prepare_reference_budget.py \
  --output benchmark/.data/rb-new --memory-mib 16384 --cpu-affinity 0,1 \
  --cpu-placement cpuset
```

The helper requires a Linux cgroup v2 user manager, Docker/rootlesskit and pasta. Strict CPU placement also requires cpuset delegation to that user manager. It retains exact commands, actual `memory.max`, zero-swap, two-core quota and effective cpuset readings, and daemon identity in `setup.json`. It creates only uniquely named private units and changes no global delegation. Import prepared images into the recorded endpoint before timing, then launch the reference runner through `systemd-run --user --slice=<recorded-slice>` and `taskset --cpu-list 0,1`. Supply the recorded `--docker-host`, `--docker-root-pid`, `--resource-budget <recorded-cgroup>`, `--budget-memory-mib 16384` and `--budget-cpu-placement cpuset`. Run a separate capability preflight for every backend before a formal cohort.

`--cpu-placement affinity` remains a diagnostic fallback; pass the matching `--budget-cpu-placement affinity` to the runner. It requires whole-parent quota and observed thread affinity, but cannot establish complete fixed-CPU placement when a runtime resets affinity. For example, [runc 1.5.1 resets the container init affinity after cgroup configuration](https://github.com/opencontainers/runc/blob/v1.5.1/libcontainer/process_linux.go#L784); the later payload helper cannot constrain that earlier interval. The runner rejects observed escapes and OOM events. Affinity violation evidence retains offending thread identities and CPU lists; exits and changing thread sets remain unknown. Live snapshots alone do not prove every short process lifetime. Verify complete runtime scope and inheritance independently before claiming matched resource enforcement in public comparisons. Parent memory includes charged cache and private infrastructure; shared cache already charged elsewhere is excluded, and its lifetime memory peak is not a per-task peak.

Use `--resource-observation sampled` only for separate capability/resource probes. Formal latency cohorts use the default `--resource-observation off`: periodic process/thread scans compete for the same CPU budget and alter timing. Unobserved RSS is `null`, and missed process lifetimes remain unknown. Whole-parent before/after accounting is retained separately; a successful unobserved timing does not establish scope coverage.

Prepared rootfs bytecode is archived before tar/image/disk construction. Every Python payload uses `PYTHONDONTWRITEBYTECODE=1` and the absent `PYTHONPYCACHEPREFIX=/__pvisor_reference_no_pyc__`; disabling writes alone still permits loading existing host bytecode. Both variables pass explicitly through pVisor `--pass-env`. Actual parent and filesystem-child flags must match. The runner verifies complete input file bytes, permissions and symlink targets before warmups and after sampling, with directory permissions for newly generated manifests. Independently compare OCI contents and guest-disk contents with the common rootfs; an immutable image ID alone does not establish equality. Failed input gates retain their reports and cannot supply formal performance results.

`--tool-scratch executor` is the default: preserve each executor's provided `TMPDIR` (native fallback `/tmp`) and create an empty private `.data/pvisor-reference-*` directory under it. Private HOME, the fixed repair’s Cargo home and an enabled Node compile cache use that directory; nothing reuses another task's cache. The `env` preflight records Node's actual API status, base/version directory and `statfs` storage type. Different default temporary storage is part of the configuration cost, not a pure VMM comparison. Node environment overrides and inherited reference-cache markers from the host are removed; nested tool actions may reuse only their current task's explicit directory. Parent and all seven workers must report the declared scratch policy and matching environments. The seven-operation Cargo subprocess instead uses fresh workspace-local `_cargo-home`, `_tmp` and `_cargo-target` directories in every backend; worker cache fields describe the inherited environment, not this Cargo override. The fixed repair may reuse its cache between npm calls within one task; persistent shared caches are outside this policy.

`--tool-scratch workspace` is a separate storage control using `_reference_tmp` in each fresh workspace. Existing fixture caches and foreign/symlink directories fail before sampling. Keep executor-default and workspace controls in different outputs and statistics; page-cache warmth is also separate. Do not overwrite an executor-provided tmpfs path with `/tmp` or present the workspace control as default product performance. Reprepare inputs when changing the harness or cache policy, and rerun full OCI/ext4 equivalence gates before preflight.

The registered write operation creates 256 files of exactly 64 KiB each. Complete byte validation occurs after the operation timer and remains included in task completion; direct and staged host outputs are independently reread. Successful workspaces, stage uppers and reference-VM disks remain under the trial directory for publication audits. Check available storage before a complete cohort; reflink/sparse copies can acquire private blocks during execution. Changed workload or cache policies require fresh prepared rootfs/image/disk inputs and their equivalence audits.

### Rebuilding and repeating the complete matrix

Freeze the actual source files, including local edits, before building release binaries. Retain the source manifest, compiler identity, build command and log beside the binaries. Pass `--build-receipt` to the reference runner: its JSON must contain `pvisor_sha256`, `source_manifest_sha256` and `source_identity.head`, with `source-manifest.json` in the same directory. The runner rejects a different binary or manifest. Without a receipt the binary source is explicitly unknown; the runner's repository HEAD does not establish binary provenance.

Prepare the tool environment again and retain an input manifest for its files, symlinks, firmware, reference kernel/config, disk and image. Pin one immutable harness for all cohorts. Run startup, filesystem and fixed repair into separate short output directories under `.data/`, with 60 samples and three warmups per backend. Long job paths can exceed Unix socket path limits; a failed setup is a failed preflight, not a latency result. Run the environment and real-CLI cases separately under B-AGENT-TASK as well. Keep output correctness, Bundle isolation, untouched-original and complete staged-write checks enabled.

Use a fresh private Docker data root and verify the actual storage driver with `docker info`. For the classic-driver comparison, rootless `overlay2` is supported on suitable recent Linux hosts; `fuse-overlayfs` is a fallback when kernel overlay is unavailable. VFS is intended primarily for testing and must not stand in for the usual container storage configuration. Docker Engine 29 also supports its default containerd image store; declare which store is tested. See the [Docker storage-driver documentation](https://docs.docker.com/engine/storage/drivers/select-storage-driver/).

Finish all uninstrumented sampling before running profiles, kernel builds, tests or resource experiments. A complete retest also covers apply/conflict/interruption recovery, local network controls, idle and useful-task density, isolation, supervision, replay, and platform-specific tools/memory tests. Do not use old rows to fill a missing condition. Physical-memory conclusions must include VM backing/cache and compression-store/pool memory, recovery must pass data-integrity checks, and completed tasks must accompany density figures. Live cold-page compression and compressed execution snapshots are different conditions. Firmware comparisons need matching current binaries, frozen configs and workload capability checks before timing.

## Complete Ubuntu controls

`prepare_ubuntu_reference.py` prepares an official cloud disk with verified checksums, a generic kernel/initrd and tools. `ubuntu_baselines.py` checks systemd, networking, cloud-init, SSH readiness and workload correctness. It compares image-free pVisor with `firecracker-ubuntu`, `qemu-ubuntu` and `qemu-microvm-ubuntu`; first cloud-init boot is a separate case. This answers deployment waiting, not VMM overhead.

```bash
python3 benchmark/pvisor/prepare_ubuntu_reference.py --help
python3 benchmark/pvisor/ubuntu_baselines.py --help
python3 benchmark/pvisor/render_ubuntu_baselines.py --help
```

Use a frozen binary, new `.data/` outputs, matching CPU/tool-VM budgets and independent cohorts. Do not turn a failed preflight into a zero-latency sample or pool template/configuration changes.

## Task, resource and correctness suites

B-WORKFLOW measures the full machine workflow for sparse changes, against Git worktree and a native reflink copy. It includes private-view creation, executing twenty edits, reviewing their content diff, applying ten paths and disposing the remaining view. Normal application and a host-conflict refusal are independent cases. Input repository construction and identical trial resets are outside timing; creating each backend's task view is inside timing. The fixture is committed and packed before sampling; Git automatic maintenance is disabled. Both controls use `git diff` to select a patch and `git apply --check` before application. Git/reflink controls are native processes; this is a workflow comparison, not equal security enforcement.

```bash
python3 benchmark/pvisor/review_workflow.py \
  --binary /absolute/path/to/frozen-release/pvisor \
  --binary-source-commit <actual-binary-source-commit> \
  --source-manifest /absolute/path/to/binary-source-manifest.json \
  --build-receipt /absolute/path/to/build-receipt.json \
  --output benchmark/.data/workflow-new \
  --sizes 100,10000 --cases normal,conflict \
  --cpu-affinity 0,1 --samples 30 --warmups 3
```

Publish the complete report with:

```bash
python3 benchmark/pvisor/publish_review_workflow.py \
  --report benchmark/.data/workflow-new/report.json \
  --output docs/src/en/benchmarks
```

Copy the derived CSVs to the Chinese article directory after reviewing both articles. First run a separate `--samples 1 --warmups 0` preflight. Every trial verifies all original files before application, the twenty complete content diffs, selected-only final changes or complete refusal, and the pVisor Bundle's rootless isolation and staging evidence. Reflink uses `--reflink=always`: an unsupported filesystem fails rather than silently becoming a full-copy control. All valid slow samples and failed trials are retained. Raw reports, frozen binary/harness, command logs and input manifests stay in the output `.data/` directory. Publish complete cohorts and bootstrap intervals for median differences; do not infer human review savings or VM/container performance from this test.

`product_v1.py` and its active `v1/` modules serve B-APPLY, B-NETWORK, B-ISOLATION, B-SUPERVISION and B-REPLAY. The versioned module name is a report/runner contract, not a retired product feature. It pins executable inputs, validates Bundle isolation and preserves failures. Apply uses Git patches as a control; network uses native and host-network Podman; use `density.py` for fixed-budget density. That runner does not time a Git-review control or full rollouts; use B-WORKFLOW for the complete local review workflow comparison, and keep full-rollout claims unmeasured.

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output benchmark/pvisor/.data/tasks-new \
  --build-receipt /absolute/path/to/build-receipt.json \
  --cpu-affinity 0,1 --samples 30 --warmups 3 \
  --suites network
```

Run each registered ID in a separate invocation: `network`, `apply,baselines`, `isolation`, `replay` or `supervision`. Mixing IDs is rejected. Current host/staged jobs must report rootless isolation; staging additionally requires read/write enforcement evidence. The superseded filesystem entry is removed from this driver; the reusable fixture helper remains for the reference runner. Use `--help` for suite-specific counts and guards. Every apply file count uses the full configured sample and warmup counts. SIGKILL and concurrent-conflict probes use up to three independent repetitions per requested state or condition; replay uses three independent repetitions. These correctness repetitions do not establish tail latency. A guard is not a failure or a completed job. SIGKILL probes must hit the requested state; missed windows are retained separately. Snapshot/standalone CLI stress scripts are removed because that product entry is retired; use `just test-service-vm` for the current environment-sharing correctness gate.

B-APPLY `apply,baselines` interleaves stage apply/drop/conflict, native copy and Git apply within seeded paired rounds, using the configured file counts. For the full scale comparison, pass `--apply-sizes 10,1000,10000,100000 --samples 30 --warmups 3`: four sizes, five operations and 30 formal rounds produce 600 timing conditions, with 60 warmups excluded. There are no file-count-dependent timing sample caps. Creating changes, patches and targets is outside timing; every target is fully verified. Failed measured conditions retain their evidence while unrelated conditions continue.

B-DENSITY uses `density.py`, replacing the former one-second occupancy module. Each batch runs in a fresh owned systemd cgroup with a two-core quota, fixed memory.max and zero swap. Every payload installs and verifies its requested CPU affinity, including OCI runtimes that reset inherited masks. Every ready task waits on stdin before release, so startup staggering cannot masquerade as simultaneous occupancy. Idle Python and useful Python/Git tasks are separate: useful tasks touch/checksum 32 MiB, modify four of 64 committed files, run actual Git status and verify all contents. Stage/VM originals and retained changes must both pass checks. `prepare_density_env.py` prepares `/bench/density-worker.py`, Python/Git, a complete input manifest and a private Podman overlay image/store. Podman uses disabled inner cgroups and both payload/conmon membership must match the enclosing budget; see the [Podman run documentation](https://docs.podman.io/en/latest/markdown/podman-run.1.html#cgroups-how).

```bash
python3 benchmark/pvisor/density.py \
  --binary /absolute/path/to/frozen-release/pvisor \
  --build-receipt /absolute/path/to/build-receipt.json \
  --firmware /absolute/path/to/libkrunfw-directory \
  --rootfs /absolute/path/to/prepared-density-rootfs \
  --input-manifest /absolute/path/to/input-manifest.json \
  --podman-root /absolute/path/to/private-podman-store \
  --podman-image <immutable-image-id> \
  --output benchmark/.data/density-preflight \
  --concurrencies 1,2 --samples 1 --cpu-affinity 0,1
```

Run a separate full sweep only after the preflight passes. Keep memory/RSS sampling, attempted/ready/validated completion counts, failed batches and OOM events; a killed reporter is missing evidence, not a successful capacity result. Whole cgroup memory includes private VM backing and charged cache, while shared prepared tool/image caches may remain charged outside it. This measures capacity under the stated cgroup limit, not whole-machine net memory savings or compression. Compression requires its separate integrity-checked experiment. Root paths must remain short enough for Unix control sockets.

After sampling ends, `publish_density.py --report <complete-report.json> --output <locale-benchmark-directory>` verifies retained source/binary/input receipts, the exact harness inventory and every attempted condition. Staged/VM results must match their retained ready/result output and unique successful Run/Attempt IDs; a time-based worker token alone is not globally unique. Original reports remain unchanged and the supplementary execution-evidence audit stays under `.data/`. Five complete rounds are required; lost reporters remain unknown tasks, while failed and OOM batches remain in the summary. Full-batch timing includes only complete no-OOM batches. Whole-cgroup barrier memory and observed peak ranges accompany completion counts; five observations do not establish tail latency or sustained reliability. Idle and useful cases stay separate, and a single successful batch cannot establish a supported concurrency limit.

`parked_density.py` is a separate B-DENSITY cohort for dormant recoverable states, not active-task capacity. Compile `parked_memory_probe.rs` once from frozen source using `rustc --edition=2024 --target x86_64-unknown-linux-musl -C opt-level=3 -o <worker> <frozen-source>` and retain a receipt with `worker_sha256`, `source_sha256`, `target`, compiler identity and command. `prepare_density_env.py --parked-worker <worker> --parked-worker-receipt <receipt>` adds the exact static bytes before hashing/importing a fresh image. Do not alter the already prepared active-density inputs.

Run the separate parked preflight after all formal timing ends:

```bash
python3 benchmark/pvisor/parked_density.py \
  --binary /absolute/path/to/frozen/pvisor \
  --build-receipt /absolute/path/to/build-receipt.json \
  --worker /absolute/path/to/frozen/parked-memory-probe \
  --worker-receipt /absolute/path/to/worker-receipt.json \
  --firmware /absolute/path/to/libkrunfw-directory \
  --rootfs /absolute/path/to/fresh-parked-inputs/rootfs \
  --input-manifest /absolute/path/to/fresh-parked-inputs/input-manifest.json \
  --podman-root /absolute/path/to/fresh-parked-inputs/podman-store \
  --podman-image <immutable-image-id> \
  --output benchmark/.data/parked-preflight \
  --concurrencies 1,2 --samples 1 --cpu-affinity 0,1
```

Every state touches/checks 64 MiB, waits for an exact stdin token, and on recovery verifies the saved identity, full data, Git status and four edits among 64 files. Admission/parking is sequential, all states must remain parked at the common barrier, and recovery uses one fixed slot. Current raw/compressed Job snapshots, native SIGSTOP and Podman pause have different portability and boundary semantics. Podman needs delegated child cgroups to freeze; payload/conmon must remain below the same 2 GiB/two-core/zero-swap parent. Unsupported stdin restoration or freezer control is a failed preflight, never a timed-sleep fallback. Only after every selected preflight condition passes, start a new five-round 1..128 capacity sweep. This does not establish tail latency, active concurrent throughput, SDK offload or automatic cold-page compression; raw backing cache may be reclaimed under the same memory pressure. Keep all failed and unknown outcomes, OOM events and private backing/cache in the accounting.

B-CLUSTER is retired with the Controller/Worker implementation. Removed runners are `cluster_scalability.py`, `cluster_worker.py` and `controller_history.py`; their dedicated tests, `plot_cluster_scalability.py` and `publish_controller_history.py` are also removed. There is no current reproduction, plotting or publication command for this ID. Frozen historical harnesses and reports in local `.data/` remain archival evidence, not active entry points. The locale `cluster-scalability.md` articles identify the retained CSVs as historical results; those bytes and receipts must not be relabeled as daemon measurements.

B-DENSITY and B-VM-MEMORY remain independent local benchmarks: `density.py`/`density_worker.py`, `parked_density.py`/`parked_memory_probe.rs`, `vm_memory.py`/`memory_probe.rs`, `live_vm_memory.py` and `macos_cold_ram.py` are not Cluster runners. Their source/binary/input receipts, integrity checks, failure accounting and resource controls remain required. None establishes daemon density, scheduling throughput or retained-history costs.

## macOS

B-STARTUP uses `vm_ready.py`; B-MACOS uses `macos_docker_tools.py` for Docker Desktop and pVisor tool comparisons. B-VM-MEMORY uses `macos_cold_ram.py` with data-integrity checks. `macos_migration.py` is a separate engineering A/B runner. Keep Apple Silicon/HVF data separate from Linux/KVM. Cold guest residency and footprint do not establish net physical savings. Do not reuse the deleted standalone snapshot harnesses with current binaries.

## Linux execution-snapshot memory

`vm_memory.py` serves B-VM-MEMORY through the current Job `suspend`/`resume` API. Prepare a small rootfs and compile `memory_probe.rs` into `/bench/memory-probe` with the Rust musl target. Keep the source, compiler command and rootfs manifest under `.data/`. The probe touches and checks every byte of repeated or deterministic random private memory, and verifies the same execution token and checksum after restore.

```bash
python3 benchmark/pvisor/vm_memory.py \
  --binary /absolute/path/to/frozen-release/pvisor \
  --build-receipt /absolute/path/to/build-receipt.json \
  --firmware /absolute/path/to/libkrunfw-directory \
  --rootfs /absolute/path/to/prepared-memory-rootfs \
  --output benchmark/.data/memory-preflight \
  --samples 1 --warmups 0 --cpu-affinity 0,1
```

An owned systemd user service installs a 2 GiB memory limit, zero swap allowance and two-core CPU quota. Actual controls are verified before launch. Memory measurements cover the entire cgroup, including helpers, capture/restore and charged backing/file cache. Raw/compressed storage and repeated/random payloads alternate in randomized order. A failed suspension or checksum is a failed trial, never a memory-saving result. Use a fresh output for formal samples only after all four preflight conditions pass. Snapshot storage compression is different from a live cold-page pool; this single-VM experiment does not establish concurrent task density or a Docker/other-VM advantage. Host resume-to-completion includes remaining guest sleep; the guest's first complete scan is a separate metric.

## Paired kernel configuration experiment

B-KERNEL-ENG rebuilds both firmware configurations from a frozen libkrunfw source, the same Linux 6.12.109 tarball/patches and the same compact packaging code. The baseline config reference must precede trimming; it selects config text only, never an old binary. The candidate uses the frozen current configuration. All actual Kconfig outputs and changes, compiler/build logs, kernel/library/source digests are retained. Required SMP, KVM guest, CPU mitigations, namespaces, seccomp and virtio-fs/network features must remain enabled.

```bash
uv run --no-project --with pyelftools==0.33 python benchmark/pvisor/prepare_firmware_comparison.py \
  --libkrunfw-root /absolute/path/to/libkrunfw \
  --baseline-config-ref <full-commit-before-config-trimming> \
  --output benchmark/.data/firmware-new --jobs 8
python3 benchmark/pvisor/kernel_comparison.py \
  --assets /absolute/path/to/prepared-reference-assets \
  --binary /absolute/path/to/frozen-release/pvisor \
  --build-receipt /absolute/path/to/product-build-receipt.json \
  --firmware-receipt benchmark/.data/firmware-new/build-receipt.json \
  --baseline benchmark/.data/firmware-new/baseline \
  --candidate benchmark/.data/firmware-new/candidate \
  --output benchmark/.data/kernel-preflight --samples 1 --warmups 0
```

Build only after unrelated timing ends. After all shell/tool preflight conditions pass, run into a fresh short `.data/` directory with 30 samples and three warmups. Configuration A/B is engineering evidence, separate from user runtime rankings. The runner verifies every frozen firmware source/build-input byte, actual firmware/kernel/config identities and required enabled kernel options, then checks the complete shared tool inputs before and after all tasks, including failed warmups. Raw outputs must stay under `.data/`. Default tool caches preserve executor TMPDIR; select `--tool-scratch workspace` only as a separate control. An established private parent can be declared with `--resource-budget`, `--budget-memory-mib`, `--budget-cpu-placement` and `--resource-observation`; sampled live checks retain unknown lifetimes and do not establish complete CPU placement or capacity. Keep measured binary source identity separate from runner HEAD. Report removed guest device/LSM capabilities and validate networking/suspend/restore separately; shell startup alone establishes none of those capabilities. Capability checks should retain three independent repetitions per condition: checked local small/bulk/stream HTTP and a deny-all direct-socket negative control, plus raw/compressed snapshots of repeated/random private memory with the same token and full checksum after restore. Use the versioned `v1/network_worker.py`, network origin and `vm_memory.py`/`memory_probe.rs` helpers, retaining exact commands/source/input manifests; tiny capability cohorts do not provide network latency, physical-memory or density rankings. The paired bootstrap compares matching rounds, and separated distributions remain separate.

## Engineering and diagnostics

### Extended Linux HOST API kernel cache

`kernel_cache_runner.py` / `kernel_cache_driver.rs` serve **B-FS-ENG**;
`--profiles` serves independent **B-FS-DIAG**. The [engineering report](KERNEL_CACHE_REPORT.md)
and derived [timing CSV](kernel-cache-summary.csv) / [counter CSV](kernel-cache-counters.csv)
do not replace or modify the immutable-lower-cache experiment below. This is a direct
Linux HOST API experiment, not `pvisor run`, VM, journaling or review guarantees.

```sh
python3 -m pytest benchmark/pvisor/test_kernel_cache_runner.py benchmark/pvisor/test_kernel_cache_report.py -q
python3 benchmark/pvisor/kernel_cache_runner.py --build \
  --output benchmark/pvisor/.data/kernel-cache-build-new
python3 benchmark/pvisor/kernel_cache_runner.py \
  --build-receipt benchmark/pvisor/.data/kernel-cache-build-new/build-receipt.json \
  --output benchmark/pvisor/.data/kernel-cache-preflight-new \
  --samples 1 --warmups 0 --seed 4207 --affinity 0,1
# Announce the formal timing window; no concurrent builds/tests or editor cargo checks.
python3 benchmark/pvisor/kernel_cache_runner.py \
  --build-receipt benchmark/pvisor/.data/kernel-cache-build-new/build-receipt.json \
  --output benchmark/pvisor/.data/kernel-cache-timing-new \
  --samples 30 --warmups 3 --seed 4207 --affinity 0,1
# Only after formal timing exits; never merge diagnostic elapsed times.
python3 benchmark/pvisor/kernel_cache_runner.py \
  --build-receipt benchmark/pvisor/.data/kernel-cache-build-new/build-receipt.json \
  --output benchmark/pvisor/.data/kernel-cache-profile-new --profiles \
  --samples 3 --warmups 0 --seed 4207 --affinity 0,1
python3 benchmark/pvisor/kernel_cache_report.py \
  --timing benchmark/pvisor/.data/kernel-cache-timing-new/report.json \
  --profiles benchmark/pvisor/.data/kernel-cache-profile-new/report.json \
  --output benchmark/pvisor/.data/kernel-cache-publication-new
```

Every output must be NEW. The isolated offline release build freezes dirty source
bytes, vendor dependencies, compiler commands, binary and harness SHA-256 receipts;
its default target directory is `benchmark/.data/kernel-cache-target`. One binary
selects all five conditions through `api::OverlayMountConfig.kernel_cache`:
legacy-writable uses default `Disabled`/1s, metadata-writable and metadata-readonly
use `Metadata`/60s, metadata-and-data-readonly uses `MetadataAndData`/60s. Every
physical lower is `Immutable` with the physical cache enabled. Enabled policies
use explicit `OwnedViewContract { exclusive_upper_and_work: true,
fixed_metadata_and_aliases: true }` and `StableView`; every mount has owner-only
access and `default_permissions=true`. No journal, preimage, observation metrics,
custom policy or exclusions are configured. Writable metadata requires its real
fusectl abort endpoint; there is no guard bypass, namespace replacement or fallback.

All native/lower/upper/work backing is genuine **noatime tmpfs**, mounted with
`noatime,nosuid,nodev,mode=0700,size=512m` in a new private user/mount/PID namespace.
The coordinator is PID 1; namespace uid 0 maps only to the invoking host user.
`unshare --kill-child=KILL --propagation private --mount-proc` prevents exported
mount propagation and ensures init/supervisor death kills all contained consumers,
including tools in separate sessions. No external view/FD/bind/namespace aliases
are allowed. Driver `has_exited()` watchers (5ms), active-server supervision while
waiting (50ms), bounded shutdown and an outer 1800s lifetime enforce the contract.
Process abort alone is not cache/FD revocation for external users; such users are
outside this experiment and forbidden. Outer host-wide build/test checks run about
every 250ms even though the inner private `/proc` only sees contained processes.
The `containment-receipt.json` records commands, namespace identities, deadlines,
checks, exit status, surviving users and host mount state.

Live mountinfo/device proofs cover every lower/upper/work/native instance. A physical
file and directory with old atime (1s after epoch, mtime 2s) undergo 20 reads/listings
and must retain exact atime; future atime is not used to conceal relatime behavior.
Both namespace-init death and unshare-supervisor death were separately exercised
with independently sessioned descendants and zero live users left. These are
containment tests, not injected notifier-failure tests. Every accepted cohort
normally detaches its FUSE mounts, archives backing with xattrs/ACLs/numeric ownership
outside all timers, hashes that archive, then normally unmounts tmpfs. Live inode/dev
identities remain in inventories; extraction cannot recreate those original IDs.
A fatal pre-archive failure may lose ephemeral tmpfs contents but durable logs remain.

The 2048/32 half-deep fixture is independent of the unchanged immutable-cache
experiment below. No global host mount flags change. Inputs are inventoried
before/after; native writes use a private copy. **Old kernel-cache Btrfs/future-atime
cohorts are unaccepted historical diagnostics** because upper backing atime violated
`fixed_metadata_and_aliases`. Raw remains intact; old report/CSVs/harness and the
status sidecar are retained under `.data/kernel-cache-btrfs-history-20261007/`.
They must not be pooled with or used as an A/B baseline for noatime tmpfs.
Overlay instances own fresh upper/work/mountpoint and the immutable lower remains
stable until all sessions detach. Persistent timing mounts receive immediate priming
before hot/readsearch/TTL commands. `ttl` waits 1.1s outside the operation timer:
legacy metadata expires while extended 60s metadata remains warm; it does not test
60s expiry. `whole-tools` times a new process/mount/verified git+rg+byte-read/unmount
per sample, including coordinator launch/shutdown and backing-proof/supervision
bookkeeping. Namespace/tmpfs preparation and archival are outside task timers.
Warmups are retained separately.
All samples must pass byte/tool validation; after sampling the persistent conditions
must pass mutation or readonly namespace/EROFS probes before the cohort is accepted.
These probes are narrower than the current crate contract/mount tests; they are
not a replacement for those tests or fault-injected notifier failure tests.

Profile mode creates one fresh process/mount per count case, plus `prime-only`.
Warm deltas subtract the independently mounted prime-only case in the same round;
all lifecycle counts and their ranges are retained, not just deltas. Profile times
never enter formal distributions. Expected components are derived from frozen
`core.rs`, `fs.rs`, `mount.rs`, `cache.rs`: one core profile and one adapter profile
per overlay; Arc clones share state and current notifier threads construct no
profile. All actual PID/component/instance records are retained and each final is
required exactly once. Unexpected/missing instances fail the cohort. Other future
profile constructors require explicit source/lifecycle review, not a relaxed guard.

Inner build/test interference checks run before and after every case and preserve failures;
these are process snapshots, not a proof against arbitrarily short unseen activity.
Do not run builds/tests or other experiments during measurement. No slow valid
sample is dropped. Cleanup reads mountinfo rather than stat-based `ismount`, which
can miss disconnected FUSE mounts; only exact owned mountpoints are normally
unmounted, without sudo, lazy detach or policy changes. Final receipt verification
recomputes complete cohort membership, distributions/bootstrap intervals, all
profile finals, input equality and artifact hashes before exporting derived CSVs.
The earlier **pre-final-P1, limited performance evidence** commands use `kernel-cache-noatime-build-20261007`,
`kernel-cache-noatime-preflight-20261007`, `kernel-cache-noatime-timing-20261007`,
`kernel-cache-noatime-profile-20261007`, and `kernel-cache-noatime-publication-20261007`
under `benchmark/pvisor/.data/`. The two termination checks and exact commands are
in `kernel-cache-noatime-containment-check-20261007/receipt.json`. Publication also
requires noatime proofs, containment exit/no-surviving-users, backing archive digest,
complete stage proofs and an explicit `noatime-tmpfs-v1` acceptance marker; it rejects
old Btrfs cohorts rather than weakening hashes. No old/failed/preflight/profile
elapsed time enters formal statistics. Native and overlays share tmpfs characteristics;
these results do not establish disk-backed performance or durability.

Those earlier figures are **not final-P1-source acceptance**. Their raw directories
remain intact, and `.data/kernel-cache-noatime-pre-p1-history-20261007/status.json`
archives the report/CSVs with `final_fix_acceptance=false`. No cross-version pooling
or performance attribution is permitted.

The final P1 source is newly frozen under `kernel-cache-p1-final-build-20261007`,
with `source-version.json` binding Core/service/host-FS implementation hashes,
build receipt, full dirty-source inventory and binary. New preflight
`kernel-cache-p1-final-preflight-20261007` and independent profile
`kernel-cache-p1-final-profile-20261007` passed. Three formal attempts
(`kernel-cache-p1-final-timing-20261007`, `kernel-cache-p1-final-timing-quiet-20261007`,
`kernel-cache-p1-final-timing-isolated-20261007`) all failed build/test interference
checks. **No final-source 30-sample performance distribution is available.** Current
`kernel-cache-summary.csv` explicitly contains blocked cells and empty time/CI fields;
`kernel-cache-counters.csv` contains only separately validated final-source diagnostics.

Failure cleanup exposed a limitation: namespace kill can leave exiting FUSE tasks
blocked in `request_wait_answer`, so the earlier simple-process death checks do not
establish immediate bounded FUSE cleanup. Remaining-thread mountinfo precisely
identified this experiment's four live endpoints; scoped abort removed all residual
PIDs. The original failure receipts retain their survivor observations, and the
additional cleanup evidence is in `kernel-cache-p1-final-failed-cleanup-20261007/`.
No unrelated connection, global setting or admission guard was changed.

Blocked publication is deliberately separate from successful formal publication:

```sh
python3 benchmark/pvisor/kernel_cache_report.py \
  --failed-timings \
    benchmark/pvisor/.data/kernel-cache-p1-final-timing-20261007/report.json \
    benchmark/pvisor/.data/kernel-cache-p1-final-timing-quiet-20261007/report.json \
    benchmark/pvisor/.data/kernel-cache-p1-final-timing-isolated-20261007/report.json \
  --profiles benchmark/pvisor/.data/kernel-cache-p1-final-profile-20261007/report.json \
  --source-version benchmark/pvisor/.data/kernel-cache-p1-final-build-20261007/source-version.json \
  --output benchmark/pvisor/.data/kernel-cache-p1-final-publication-blocked-new
```

This path validates diagnostics and exact source/binary/harness/noatime/archive hashes,
requires the timing attempts to be failed and refused by the normal validator, and
exports no timing statistics. The retained run is
`kernel-cache-p1-final-publication-blocked-20261007`. Normal performance publication
still requires a complete passed cohort. To retry final-source timing, reuse its
frozen build receipt and the measurement command above with a **new** output directory;
keep editor auto-Cargo checks and every agent's tests stopped for the entire window.
Do not combine partial failed rows or substitute earlier P50/percentages.

### Owned immutable lower physical metadata cache

`immutable_lower_cache.py` / `immutable_lower_cache_driver.rs` serve **B-FS-ENG**;
`--profiles` serves **B-FS-DIAG**, never elapsed-time claims. The retained
[engineering report](IMMUTABLE_LOWER_CACHE_REPORT.md) compares real Linux host
FUSE native/mutable/immutable-cache-off/on. This is not a VM/OCI or user-facing
benchmark. It uses the completed API traits with CLI disabled, no journal in all
conditions, unchanged kernel TTL/KEEP_CACHE, and no permission workaround.

Run from the repository root, using a **new** output directory for each command:

```sh
python3 -m pytest benchmark/pvisor/test_immutable_lower_cache.py -q
python3 benchmark/pvisor/immutable_lower_cache.py --build \
  --output benchmark/.data/immutable-cache-build-new
python3 benchmark/pvisor/immutable_lower_cache.py \
  --build-receipt benchmark/.data/immutable-cache-build-new/build-receipt.json \
  --output benchmark/.data/immutable-cache-preflight-new --samples 1 --warmups 0
# After successful preflight, announce the timing window; no concurrent build/test.
python3 benchmark/pvisor/immutable_lower_cache.py \
  --build-receipt benchmark/.data/immutable-cache-build-new/build-receipt.json \
  --output benchmark/.data/immutable-cache-timing-new --samples 30 --warmups 3 \
  --seed 4207 --affinity 0,1
# Only after timing exits; profile timings are not included in engineering tables.
python3 benchmark/pvisor/immutable_lower_cache.py \
  --build-receipt benchmark/.data/immutable-cache-build-new/build-receipt.json \
  --output benchmark/.data/immutable-cache-profile-new --profiles \
  --samples 3 --warmups 0 --seed 4207 --affinity 0,1
```

Select allowed CPUs with `--affinity` on other hosts (default: first two allowed
CPUs). The isolated generated manifest contains `[workspace]`, frozen path
dependencies and the product's vendored fuser patch. Build is offline, release,
CLI-disabled, with four build jobs; it reuses root `target` by default, or accepts
`--target-dir benchmark/.data/immutable-cache-target` for fully isolated artifacts.
All tracked/nonignored crate and vendor files, including current dirty bytes,
are frozen and SHA-256 inventoried; build command/compiler/lock/manifest/binary
and harness receipts are retained. Existing output directories are rejected.
Fixture creation is outside timing: 2048 unique files in 32 branches, 1024 shallow
and 1024 seven-level-nested files; private Git repo with automatic GC/maintenance
disabled, optional Git locks disabled during tasks. Only generated fixture
atimes are initialized beyond the run window to preserve physical metadata under
relatime; no host mount policy is changed. Exact lower namespace, bytes, modes,
ownership, inode/device/link identities, times and xattrs must match after runs.

Each condition has an exclusively owned upper/work/mountpoint. Persistent mounts
remain alive for seeded random interleaved rounds but only one workload runs at a
time. Immediate untimed priming precedes hot and TTL-expiry operations; the latter
waits 1.1 seconds before the first traversal, outside the operation timer. Each
metadata/open/read command traverses all files twice and compares all bytes;
readsearch traverses once. `tools` runs clean `git status`, exact-path-checked `rg`
and all-byte verification; `whole-tools` uses a fresh mount per sample and measures
coordinator launch-through-normal-unmount, with mount/tools/unmount fields kept
separate. Persistent process lifetime includes all idle/shuffle/wait windows and
is not task latency. Warmups are retained but not aggregated; slow valid samples
are not discarded, paired bootstrap intervals and distribution checks are emitted.

After sampling, warmed lower paths undergo append copy-up (old contents retained),
rename, unlink/recreate and lower-only unlink. Immediate and post-TTL visible
contents, metadata, ENOENT, directory names, exact upper file inventory and whiteout
markers are checked; every physical lower remains unchanged. No preimage/review,
concurrent upper modifier, eviction stress, VM, OCI or cold-disk claim is made.
Independent profile stderr goes directly to regular files. All PID/component/
instance cumulative records are retained; only the final record per instance is
used for comparisons. Inclusive spans are not added into a total.

The coordinator bounds responses to 90 seconds, builds to 600 seconds and
shutdown to 15 seconds; the driver also has a finite 1800-second watchdog. Failure
logs and stages are retained. Cleanup addresses only the exact generated owned
mountpoint/process group, never sudo, lazy unmount or global policy changes. A
failed real mount is not a valid performance sample; preserve it and report the
FUSE gap rather than substituting a mocked view. The recorded host successfully
mounted FUSE, so no core-only fallback was needed.

B-FS-ENG engineering runners and B-FS-DIAG diagnostic helpers remain active: `filesystem_ab.py`, `filesystem_fuse_ab.py`, `filesystem_stage_ab.py`, `filesystem_stage_durability.py`, `filesystem_lazy_ab.py`, `filesystem_kernel_probe.py` and `filesystem_diagnostic.py`. The FUSE passthrough adapter is a diagnostic control without staging semantics, not a production mode. Record each engineering run’s ID and keep raw output in `.data/`; publish only when it changes a user conclusion, after a matching user-facing comparison.

After formal timing completes, `filesystem_counters.py` collects independent filesystem and fixed-repair diagnostics from the same verified current binary and inputs:

`prepare_fuse_driver.py` builds a standalone release control offline from frozen vendored fuser and a fixed libc dependency. Pass `--fuser-source`, `--product-lock` and `--product-manifest` from the measured product's frozen source when it differs from the worktree. It retains the Cargo lock, source inventory, compiler commands and binary receipt; preparation performs no measurements.

```bash
python3 benchmark/pvisor/prepare_fuse_driver.py --output benchmark/.data/fuse-driver-new
python3 benchmark/pvisor/filesystem_fuse_ab.py \
  --assets /absolute/path/to/reference-assets \
  --binary /absolute/path/to/frozen/pvisor \
  --build-receipt /absolute/path/to/product/build-receipt.json \
  --fuse-driver benchmark/.data/fuse-driver-new/bin/fuse-passthrough \
  --driver-build-receipt benchmark/.data/fuse-driver-new/build-receipt.json \
  --output benchmark/.data/fuse-counters-new --profiles --samples 3 --warmups 0
python3 benchmark/pvisor/filesystem_stage_durability.py \
  --assets /absolute/path/to/reference-assets \
  --binary /absolute/path/to/frozen/pvisor \
  --build-receipt /absolute/path/to/product/build-receipt.json \
  --firmware /absolute/path/to/firmware \
  --output benchmark/.data/durability-counters-new --profiles --samples 3 --warmups 0
```

Both runners gate the complete input inventory before and after execution, preserve failures and failed preflights, and require the frozen current CLI source receipt. The passthrough runner also verifies the complete driver source receipt and exact fuser bytes against the product manifest. Native/direct workloads may rewrite the fixture toolchain path and refresh Git index stat data; retained input bytes and indexed paths/modes/object IDs must otherwise remain unchanged. Staged lower inventories remain exact. The durability runner checks requested policy and completion seal for preflights, warmups and accepted tasks. Default caches preserve executor TMPDIR; `--tool-scratch workspace` is a separate control. `--profiles` uses regular-file stderr and produces independent diagnostic records; omit it for separate uninstrumented observations. Durability timing uses B-FS-ENG, at least thirty paired rounds with three warmups and distribution/interval reporting; its profile mode uses B-FS-DIAG. No P99 is produced. Supply a verified private parent with `--resource-budget` and matching budget options when available; sampled observations retain unknown lifetimes and do not prove strict placement. These controls do not update cross-runtime user tables.

```bash
python3 benchmark/pvisor/filesystem_counters.py \
  --assets benchmark/.data/tools-new \
  --binary /absolute/path/to/frozen-release/pvisor \
  --build-receipt /absolute/path/to/build-receipt.json \
  --firmware /absolute/path/to/libkrunfw-directory \
  --output benchmark/.data/fs-counters-new \
  --modes filesystem,tools --samples 3 --cpu-affinity 0,1
```

The collector retains complete harness/build/source receipts and verifies the full input inventory before and after jobs. It records startup phases, child CPU/page-fault/context-switch counters, and filesystem measurements; `counter-summary.csv` is the derived request/span inventory. Pass `--resource-budget`, `--budget-memory-mib` and `--budget-cpu-placement` when using a verified private parent; the matching mode still needs complete inheritance evidence before a resource claim. Diagnostic stderr is captured directly in a regular file and retained byte-for-byte: a nonblocking pipe can return EAGAIN during large profile writes and trigger a Rust printing panic. This capture is restricted to diagnostic jobs; formal timing keeps its existing capture. A result followed by an aborted process is still failed, and a new capture condition starts a separate cohort while keeping the failed evidence. RSS sampling is disabled, with missing RSS kept unknown. Each filesystem identity includes PID, component and instance. Repeated cumulative records replace earlier snapshots rather than being added. Missing final records are explicit lower bounds; missing maximum latency is unknown, not zero. Inclusive spans and overlapping workers cannot be summed into total task time. Investigate every relevant rootfs and workspace instance before attributing a tool's delay. These results belong in technical analyses, not user latency tables.

```bash
python3 benchmark/pvisor/filesystem_ab.py \
  --assets benchmark/.data/tools-new \
  --baseline /absolute/path/to/before/pvisor \
  --candidate /absolute/path/to/after/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --output benchmark/pvisor/.data/filesystem-ab-new \
  --cpu-affinity 0,1 --samples 30 --warmups 3
```

Build frozen sources with only the proposed change between versions; report paired median differences with bootstrap confidence intervals. Instrumented profile runs remain separate from performance samples. `firmware_boot.py` serves B-STARTUP-DIAG; `guest_init.py` serves B-STARTUP-ENG. `evidence_tsv.py` can read/write legacy raw formats locally; its format does not make a report suitable for public download.

## CI regression gate

B-PROCESS uses `bench.py` through `run.sh`. It checks a successful minimal Run and Bundle access, with 2 warmups/10 samples for smoke or 10 warmups/50 samples for nightly. It does not establish cross-runtime user rankings.

```bash
just benchmark
just benchmark nightly benchmark/pvisor/.data/nightly
just benchmark-compare \
  benchmark/pvisor/.data/candidate/raw-report.json \
  benchmark/pvisor/.data/main/raw-report.json
```

## Linux SDK live RAM offload

### User memory-saving choices

For a first static result with 512 MiB configured per VM, use `--static --memory-mib 512` on both `memory_savings.py` and
`memory_sharing.py`. The former measures all 16 single-instance conditions once,
without warmups, at five seconds. The latter measures the 12 four-VM conditions
once with two-second scan windows. Both check admission immediately, retain
continuous host observations and reject foreign VMs, failed budgets, integrity
or cleanup. Background builds are recorded rather than blocking the memory
reading; durations under that contention are observational, not speed rankings.
The user table reports complete-product resident physical memory as the sum of
process PSS, counting shared pages proportionally and including the compressed
store and helpers. Keep full cgroup charge, kernel and file cache alongside it;
resident PSS alone excludes unmapped cache and is not complete host cost.
Export each complete static report with its matching publisher's `--static`.
Static CSVs contain N=1 values and observed differences, with no quantiles or
confidence intervals. Keep these cohorts separate from the longer protocol.

`memory_savings.py` serves B-VM-MEMORY using the current release
`vm_memory_savings` helper. Its user questions are whether default free-page
reclaim is sufficient, whether live compression pays for its next-task cost,
and whether pausing/offloading an idle environment is worthwhile.

Build from a frozen copy of the actual working tree, including local changes:

```sh
cargo build --locked --offline --release -p pvisor --features gateway --example vm_memory_savings
python3 benchmark/pvisor/memory_savings.py \
  --example /absolute/path/to/frozen/vm_memory_savings \
  --build-receipt /absolute/path/to/build-receipt.json \
  --rootfs /absolute/path/to/prepared/rootfs \
  --firmware /absolute/path/to/frozen/firmware \
  --output /short/disk/new-preflight --samples 1 --warmups 0 --wait 5
```

The receipt binds `example_sha256`, `source_manifest_sha256`, build command,
source HEAD and dirty state. All six modes are default, cold, pause, raw,
compressed and release. Default/cold cover repeated nonzero blocks, distinct
compressible blocks, deterministic random data and 16 MiB read-hot / 48 MiB
cold mixed data; other modes cover repeated/random data. Every VM has 2 vCPU,
256 MiB RAM and 64 MiB independently checked application data. Release frees
the allocation and subsequently reconstructs and checks it. The tool task
hashes all bytes, writes/fsyncs them and verifies the complete readback.

Use a NEW output for formal samples, with defaults of 30 samples, three warmups
and a 60-second idle window. Preflight failures remain separate. Every launch
requires 30 quiet seconds; the external observer samples complete-group memory
every 50 ms and checks visible competing VM/build processes every 0.5 seconds.
VMs, stores/backing/cache and product helpers inherit one four-core, 2 GiB,
zero-swap cgroup. Monitor/coordinator CPU is outside the product footprint.
All modes retain startup/idle/recovery memory and CPU, control acknowledgements,
next complete task time and native reap evidence. Permission or integrity
failure stops sampling; it never represents successful memory savings.
After native reap and owned-service quiescence, the coordinator hashes and
removes its successful trial's disposable live RAM backing. Logical/allocated
sizes and SHA-256 remain in `result.json`; parked storage is measured before
resume. Raw reports, monitors, guards, guest logs and source receipts remain.

This probe measures one application allocation and a fixed I/O task, not real
model inference or a production concurrency limit. KSM/COW sharing uses a
separate multi-instance protocol; do not pool its samples with these runs.

After the complete formal cohort passes, export aggregate data with:

```sh
python3 benchmark/pvisor/publish_memory_savings.py \
  --report /short/disk/formal/report.json \
  --output /short/disk/derived
```

The publisher verifies every warmup and formal trial, independent payload checks,
native reap, retained binary/source/harness hashes, installed resource budgets
and external host guards. Missing, duplicated, failed or undersampled conditions
are rejected. `memory-choices.csv` contains phase memory, peak, CPU, storage and
next-task distributions; `memory-choices-comparisons.csv` contains paired median
differences against the running default with 5,000 bootstrap resamples. Publish
these aggregate CSVs with the provenance CSV only after reviewing both locales.
Separated clusters replace a single P50, P95 is descriptive, and no P99 is emitted.

The user strategy comparison uses `memory_sharing.py --static --strategies
--memory-mib 512` with the frozen `vm_memory_scale` example and its build receipt.
It measures nine conditions: three content patterns, each with four independent
fresh VMs, four shared-snapshot COW restores, and four independent fresh VMs with
KSM advice. Both unshared and KSM arms use private anonymous RAM through the SDK
`with_private_ram()` control, with advice enabled only for KSM. All have matching guest/workload budgets, two-second
scan windows and N=1. Compare ready and post-write phases separately against the
fresh unshared arm. KSM advice is not a guaranteed merge; require positive accepted bytes for
every VM and mergeable flags for four VM processes, and retain actual RAM KSM
bytes. All-skipped shared mappings are invalid KSM evidence. This protocol does not substitute the older independent-snapshot-copy
control or restored dynamic-private KSM observations.

To compare the fourth strategy, append `--pool-daemon /absolute/frozen/pvisor-daemon`
and `--pool-receipt /absolute/path/to/daemon/build-receipt.json`. The daemon receipt
binds `binary_sha256` and the adjacent source manifest. The worker starts the
actual daemon pool component, attaches four private-RAM VMs, includes its PSS and
cgroup charge, records pool objects/encoded bytes and reaps it after VM cleanup.
This creates twelve conditions. Append `--ksm-scan-seconds 60` to extend only
the fresh KSM arm to 60 seconds; other arms keep the two-second static window.
The complete twelve-condition cohort is rerun, and the distinct windows are
retained in raw conditions and provenance. Use `--group-memory-max 4294967296` for **all**
four strategies to match the retained pre-fix comparison. The Linux daemon-pool arm preserves sparse RAM and scans resident 4 KiB pages.
It admits cross-session duplicate candidates into a read-only, size-sealed memfd
with full-byte checks, then maps them privately after CPU/device quiescence and
a live-byte recheck. Reads retain sharing, writes use COW, and scanning releases
obsolete references. Unique candidates hold bounded hashes only, not page payloads.
This arm does not require userfaultfd or compress unique pages. Local compression retains 64 KiB blocks. Guest capacity stays 512 MiB and
savings still use matching-phase unshared PSS, never the resource ceiling.
`--preflight --arm daemon-pool --pattern repeated` is a short diagnostic subset;
partial/preflight reports cannot be published. Failed runs retain evidence.
The worker streams complete `/proc/PID/smaps` into SHA-256-bound sidecars under
the trial `smaps/` directory, retaining only totals and file references in RAM.
It flushes, syncs and requests cache discard for every MiB of evidence, bounding
the measurement tool's own memory contribution when page sharing creates many
VMAs. Process PSS comes from `smaps_rollup`, avoiding per-VMA KiB rounding;
per-VMA totals remain separate evidence. Validators reopen all sidecars and
verify bytes, hashes, rollup/per-VMA totals and KSM flags; retirement preserves them. Do not mix earlier in-memory-smaps samples
with this complete rerun. The external cgroup/host observer remains outside
the measured group.

`memory_sharing.py` is the separate B-VM-MEMORY user protocol for shared
baselines and KSM. It uses `vm_memory_scale` with 2 vCPU per VM and genuine
independent-inode controls, preserving the original engineering runner's
defaults. A copied sealed baseline has identical RAM bytes and independent
physical inodes; it is not an independently captured VM. Whole-group memory
includes capture, backing/cache and shared-store helpers. Treat shared baseline
ready and dynamically dirtied private pages as different comparison boundaries.

```sh
cargo build --locked --offline --release -p pvisor --features gateway --example vm_memory_scale
python3 benchmark/pvisor/memory_sharing.py \
  --example /absolute/path/to/frozen/vm_memory_scale \
  --build-receipt /absolute/path/to/build-receipt.json \
  --rootfs /absolute/path/to/prepared/rootfs \
  --firmware /absolute/path/to/frozen/firmware \
  --output /short/disk/sharing-preflight --preflight
```

The 36 conditions cover 1/2/4 VMs, repeated/shared-random/unique-random payloads,
independent/shared baselines and dynamic KSM advice off/on. It requires the
already enabled scanner, never changes global KSM, and checks full payloads,
25/100% private writes, peer isolation and independent exit. Use another new
output without `--preflight` for 3 warmups/30 paired formal groups and 30-second
scan windows. Reaching a scan deadline without merging is a valid observation.
The external observer, resource and host guards match the single-instance
protocol. After native reap and owned-unit quiescence, runtime stores are
inventoried with bytes/modes/hashes and removed; raw results and logs remain.
Do not publish B-MEMORY-SCALE engineering runs as this user protocol.

Export only a complete formal sharing cohort:

```sh
python3 benchmark/pvisor/publish_memory_sharing.py \
  --report /short/disk/sharing-formal/report.json \
  --output /short/disk/sharing-derived
```

The publisher requires all 36 conditions and every warmup/formal group, matching
arms, budgets, retained inputs, source/binary/harness receipts and successful
correctness, reap and host guards. `memory-sharing.csv` keeps baseline sharing,
dynamic private-page deduplication and subsequent writes separate; paired
comparisons use shared minus independent or KSM-on minus KSM-off in the same
round. The provenance CSV retains input and host verification. A fixed scan
window measures observed savings, not eventual merging or production density.
Guest timing uses each group's median per-VM checked scan or write duration;
25% and 100% writes remain separate. It is not the elapsed time for the whole
group and does not include the single-instance protocol's full write/fsync task.

`live_vm_memory.py` serves B-VM-MEMORY using the current public SDK `RunHandle.offload` and `resume` API, through the `vm_live_memory_bench` example. It parks the running VM with either raw or compressed RAM backing; this is a separate mechanism from execution snapshots and automatic cold-page reclaim. Build the example from frozen source and retain its build receipt (`example_sha256`, `source_manifest_sha256`) and source manifest. Prepare the same Python rootfs for all conditions.

```bash
cargo build --locked --offline --release -p pvisor --example vm_live_memory_bench --features gateway
python3 benchmark/pvisor/live_vm_memory.py \
  --example /absolute/path/to/frozen/vm_live_memory_bench \
  --build-receipt /absolute/path/to/sdk-build-receipt.json \
  --rootfs /absolute/path/to/prepared-python-rootfs \
  --firmware /absolute/path/to/libkrunfw-directory \
  --output /short/disk/lmp --samples 1 --warmups 0
```

Every condition creates a fresh 256 MiB VM with 64 MiB fully checked private data and checked mutable state. Repeated and seeded random payloads, raw and compressed backing, alternate randomly in paired rounds. All VM/backing/FUSE helpers and the sampler inherit one verified 2 GiB, two-core, zero-swap cgroup. Complete cgroup anon/file/kernel and CPU are recorded while active, after two seconds parked, and after recovery; a 50 ms monitor retains transient memory. Restore acknowledgement, heartbeat and first complete guest scan have separate timing boundaries. All failures and OOMs remain in evidence. Use disk storage, outside tmpfs, and a new output for 30 formal samples after four preflight conditions pass. This single-VM measurement does not establish higher concurrent density or an advantage over Docker.

Host quiet admission and sampling interference rejection are mandatory and recorded in `report.json` before launch. Before **every** condition, the coordinator requires 30 continuous seconds without visible same-user KVM users or build/test processes, within a 180-second admission bound; competing work resets the window. During the service attempt, a coordinator thread **outside the measured cgroup** checks every 0.5 seconds, with checks immediately before launch and after completion. Only the UUID-owned service cgroup and its descendants are excluded, never the coordinator's parent cgroup. No additional host memory observer is placed inside the product footprint. Any detected foreign VM/build during sampling rejects that condition and stops the campaign, retaining previous rows only as diagnostic evidence: do not cherry-pick, replace rejected conditions, or pool partial formal cohorts. Admission timeout or guard failure also stops the campaign; owned-service teardown remains required.

Retain `prelaunch-wait.jsonl`, `host-guard.jsonl`, launch/result JSON, and final service-quiescence evidence with the raw cohort. The guard inspects same-user command lines and visible KVM FDs; inaccessible process/FD errors are logged, so this is not proof of host-wide quiet, and jobs shorter than the polling interval may be missed. Coordinate an otherwise idle host before rerunning. Use a NEW short absolute disk output path (replace `/short/disk/lmp` above); retain numeric `trials/<round>-<case>` directories and keep trial paths at most 70 characters for VM socket headroom. After all four preflight conditions pass, use another NEW short output path and omit `--samples 1 --warmups 0` for the unchanged 30-sample/3-warmup formal protocol. Run focused guard tests without hardware using `.venv/bin/python -m pytest -q benchmark/pvisor/test_live_vm_memory.py`.

## Prepared inputs for network and isolation controls

`product_v1.py --suites network` and `--suites isolation` accept the pinned Python/Git rootfs and private OCI image produced by `prepare_density_env.py`. The runner verifies the complete rootfs inventory, content/modes/symlinks, host tool identities and immutable image ID. This excludes repeated tool copying and image import from the experiments without changing payload contents. Keep the prepared store's original runroot; use a new output per benchmark ID.

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/frozen/pvisor \
  --build-receipt /absolute/path/to/build-receipt.json \
  --firmware /absolute/path/to/libkrunfw-directory \
  --prepared-rootfs /absolute/path/to/prepared/rootfs \
  --input-manifest /absolute/path/to/prepared/input-manifest.json \
  --podman-root /absolute/path/to/prepared/podman-store \
  --podman-image <immutable-image-id> \
  --output benchmark/.data/network-preflight \
  --suites network --samples 1 --warmups 0 --cpu-affinity 0,1
```

B-NETWORK compares native, host policy proxy, VM and OCI host-network controls, with payload CPU affinity installed and checked. All small/bulk/stream/backend conditions alternate randomly each paired round. The local origin is outside the payload CPU budget and memory is not identically capped. Full payload checks, direct-socket denial controls and installed Bundle evidence precede publication. Failures remain separate from latency. After all available preflight conditions pass, use a fresh output with 30 samples and three warmups. For B-ISOLATION, replace the suite/output: three fresh repetitions per backend are correctness evidence, with native positive controls, inside-view read/write/socket positives and outside-view negatives; these are specific boundary tests rather than comprehensive security assurance.

B-SUPERVISION uses `--suites supervision`, without OCI preparation. Stage and Git worktree conditions start with identical prepared edits; each reviews the complete content diff, applies ten files and disposes ten. The timer includes selection/extraction/check commands where needed, excluding private-view creation, task execution, fixture preparation, validation and human reading. Use a separate one-sample preflight, then 30 samples/three warmups in a new output. B-WORKFLOW separately includes view creation and execution; the two timers are not interchangeable.

Complete Ubuntu sampling additionally requires `--build-receipt` and `--firmware`. Reprepare the distribution assets into a fresh directory: the preparer records the completed raw OS disks and payload hashes after provisioning, while the runner verifies those bytes, vendor kernel/initrd and current product source receipt. Old preparation manifests without these hashes do not satisfy the current input gate. Current staged controls must install rootless filesystem enforcement and retain every checked workspace write; process-only staging does not pass. A failed full-OS or workload preflight makes the experiment exit nonzero after unaffected conditions finish. Distribution and image-free environments retain their different OS/tool capabilities; their comparison answers full-environment waiting rather than a VMM speed ranking.

`publish_vm_memory.py` generates derived summary, paired confidence intervals and provenance CSVs from complete memory cohorts. It verifies every condition/trial, restore integrity, source/binary receipts and OOM counters. Live offload and execution snapshots retain separate cohorts and metrics; missing, duplicated, failed or undersampled conditions cannot receive a successful timing table. CPU phase deltas include control/background work; after-completion snapshot memory is not restored live-VM residency. Use `--reports <live-report> <snapshot-report> --output <locale-benchmark-vm-memory-directory>` after sampling ends, then write the paired user articles from these summaries.

B-FS-DIAG `filesystem_exec_probe.py` isolates guest loader/exec paths using the same ELF bytes and every dynamic library. It compares virtio-fs file mappings with verified executable memfd copies in separate fresh VMs; both arms perform identical copying/hashing and inherit the same fd set before measured calls. `--preparation-sources original,duplicate` alternates two preparation sources: original paths prewarm their inodes, while independent workspace copies have identical bytes in distinct inodes. Duplicate preparation preserves the first mapping opportunity for the original tool; common Python preparation still warms its interpreter and shared libraries. The first exec and subsequent calls retain separate labels. This is not completely cold startup, and a zero difference in warm repeats cannot reject a first-mapping cost. Guest `/dev/shm` remains noexec. Unsupported executable memfds are a failed capability, never a reason to remount or weaken policy. It records checked `rg --version` output, per-exec child CPU/fault/context-switch counters and all final filesystem instances. Counters include common preparation; neither source is a complete Agent latency ranking.

```bash
python3 benchmark/pvisor/filesystem_exec_probe.py \
  --assets /absolute/path/to/prepared-reference-assets \
  --binary /absolute/path/to/frozen/pvisor \
  --build-receipt /absolute/path/to/build-receipt.json \
  --firmware /absolute/path/to/libkrunfw-directory \
  --output benchmark/.data/exec-probe-new \
  --samples 3 --calls 50 --cpu-affinity 0,1
```

Raw output must use a fresh `.data/` directory. The runner verifies the complete prepared input inventory before and after all conditions, freezes the CLI and firmware, and rechecks host tool/library bytes. Final verification failures retain the measured rows and make the cohort fail. Profile stderr goes directly to a regular file, with all original bytes retained; this starts a separate capture condition from earlier pipe-captured diagnostics. Host affinity and guest CPU/RAM configuration do not prove complete descendant lifetime placement or identical cross-runtime budgets.

Run only after formal timing ends. Preserve complete source/input receipts and every request identity/final span; inclusive counters cannot be summed into transport or CPU cost. Keep results in technical filesystem analysis and derived diagnostic CSVs, separate from user performance tables.


`publish_network.py --report benchmark/.data/network-new/report.json --output docs/src/en/benchmarks` requires all 17 native/host/VM/Podman/pVisor-OCI mode conditions, 30 independent batches per condition and unchanged prepared inputs. It verifies the frozen harness, input/build receipts, each retained command/output and independent Run boundary, complete response SHA-256, CPU affinity and deny-all evidence. Small-request statistics use the median within each 256-request, eight-thread batch, then distributions across batches; individual requests do not become independent samples. Transfer times exclude subsequent digest validation; job/worker times remain separate. The publisher exports same-directory summary, paired-comparison and provenance CSVs, with no P99 or public-network/model-latency claim. Failures prevent a complete comparison from being published; keep the failed report rather than replacing its cells with an older cohort.

B-APPLY's `product_v1.py --suites concurrent-conflicts` is a separate correctness probe. With at least 1,000 changed files (`--crash-files`, default 10,000), it observes a real target write, stops only its owned apply process, confirms a Prepared ledger and untouched file, injects/fsyncs an external host edit, then resumes the same apply. It requires the edited bytes to survive and a conflict exit, retaining partial-apply counts and final ledger state. A missed window is untested; silently overwritten content is a failed result. This is distinct from editing before apply and from SIGKILL recovery; none can substitute for the other. Run after formal performance timing, with three fresh repetitions, and retain every failed probe.

`publish_apply.py --report benchmark/.data/apply-new/report.json --output docs/src/en/benchmarks` exports the derived apply, recovery, comparison and provenance CSVs. It checks the retained binary/build receipt, source manifest, exact harness inventory, every planned operation/trial or known failure, and each requested versus durable crash state. Missing conditions are rejected; failed operations contribute no synthetic latency. Recovery times include only actual requested-state hits, while missed windows remain counted. At fewer than 30 pairs, Git comparison remains descriptive; P95 is absent below 30 samples and no P99 is generated. Conflict-before-apply and mid-apply external writes retain separate evidence. Validate the publisher before replacing both locale tables; complete reports and retained artifacts stay under `.data/`.

`publish_replay.py --report benchmark/.data/replay-new/report.json --output docs/src/en/benchmarks` requires all seven adapters, twenty native-format fixtures and three repetitions (420 conditions). The replay suite has no warmups. It rechecks the frozen harness, binary/source build receipt, seeded command order, exact arguments and historical observations, exclusion of future actions, zero tool execution and the unchanged pre-existing workspace. Failed conditions remain counted and contribute no successful latency. Review both locales before replacing the public `replay-fidelity.csv` (from derived `replay.csv`) and `replay-provenance.csv`; retain all raw artifacts under `.data/`. Separated timing clusters replace a single P50; P95 is descriptive and no P99 is generated. These synthetic-format checks do not execute installed Agent SDKs or compare native resume latency.

`publish_isolation.py --report benchmark/.data/isolation-new/report.json --output docs/src/en/benchmarks` requires all seven configurations and three fresh repetitions, with zero failures and unchanged prepared inputs. It verifies the retained harness, build/tool/firmware identities, seeded commands, independent Run Bundles, inside-view positives, outside accesses and exact final host/stage bytes. The derived `isolation-tests.csv` reports observed occurrence counts; it does not rank refusal latency. Review both locales before publishing it with `isolation-provenance.csv`. Original evidence remains in `.data/`; these fixtures do not establish comprehensive escape resistance.

`publish_supervision.py --report benchmark/.data/supervision-new/report.json --output docs/src/en/benchmarks` requires at least thirty paired rounds and three warmups for prepared stage/Git worktree review. It audits every retained command, full and selected diff, final twenty-file target and independent staged Run, including warmup evidence. The runner retains final target workspaces and records Git's executable digest/version before sampling. Exports are `supervision-summary.csv`, `supervision-comparisons.csv` and `supervision-provenance.csv`; review both locales before publishing. Their timer excludes task-view creation and execution, so keep them separate from B-WORKFLOW's complete-task CSVs. Raw evidence stays under `.data/`; human reading time is unmeasured.
