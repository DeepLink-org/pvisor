# pVisor benchmark runners

Read [the benchmark registry and writing rules](../README.md) before measuring or publishing. User questions, controls and roles belong to that registry; this manual owns commands and retention. Entry scripts carry `Benchmark:` declarations. Preparation, publication and plotting helpers serve their caller’s ID.

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

`B-COLD-RUNTIME-ENG` now validates the experimental Linux x86_64 kernel-fault userfaultfd pager with instance-local storage. Enable `[vm].cold_ram_compression = true` or `--vm-cold-ram-compression`; it is default-off and distinct from FUSE `ram_compression`. Userfaultfd authority must be granted by the administrator. Linux deliberately rejects external pools, file/COW backing, KSM advice and snapshot/offload combinations. The policy uses eviction/refault probing, not full read-heat tracking.

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

Build only after unrelated timing ends. After all shell/tool preflight conditions pass, run into a fresh short `.data/` directory with 30 samples and three warmups. Configuration A/B is engineering evidence, separate from user runtime rankings. Report removed guest device/LSM capabilities and validate networking/suspend/restore separately; shell startup alone establishes none of those capabilities. The paired bootstrap compares matching rounds, and separated distributions remain separate.

## Engineering and diagnostics

B-FS-ENG engineering runners and B-FS-DIAG diagnostic helpers remain active: `filesystem_ab.py`, `filesystem_fuse_ab.py`, `filesystem_stage_ab.py`, `filesystem_stage_durability.py`, `filesystem_lazy_ab.py`, `filesystem_kernel_probe.py` and `filesystem_diagnostic.py`. The FUSE passthrough adapter is a diagnostic control without staging semantics, not a production mode. Record each engineering run’s ID and keep raw output in `.data/`; publish only when it changes a user conclusion, after a matching user-facing comparison.

After formal timing completes, `filesystem_counters.py` collects independent filesystem and fixed-repair diagnostics from the same verified current binary and inputs:

```bash
python3 benchmark/pvisor/filesystem_counters.py \
  --assets benchmark/.data/tools-new \
  --binary /absolute/path/to/frozen-release/pvisor \
  --build-receipt /absolute/path/to/build-receipt.json \
  --firmware /absolute/path/to/libkrunfw-directory \
  --output benchmark/.data/fs-counters-new \
  --modes filesystem,tools --samples 3 --cpu-affinity 0,1
```

The report records startup phases, child CPU/page-fault/context-switch counters, and filesystem measurements; `counter-summary.csv` is the derived request/span inventory. Each filesystem identity includes PID, component and instance. Repeated cumulative records replace earlier snapshots rather than being added. Missing final records are explicit lower bounds; missing maximum latency is unknown, not zero. Inclusive spans and overlapping workers cannot be summed into total task time. Investigate every relevant rootfs and workspace instance before attributing a tool's delay. These results belong in technical analyses, not user latency tables.

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

Run only after formal timing ends. Preserve complete source/input receipts and every request identity/final span; inclusive counters cannot be summed into transport or CPU cost. Keep results in technical filesystem analysis and derived diagnostic CSVs, separate from user performance tables.


`publish_network.py --report benchmark/.data/network-new/report.json --output docs/src/en/benchmarks` requires all 17 native/host/VM/Podman/pVisor-OCI mode conditions, 30 independent batches per condition and unchanged prepared inputs. It verifies the frozen harness, input/build receipts, each retained command/output and independent Run boundary, complete response SHA-256, CPU affinity and deny-all evidence. Small-request statistics use the median within each 256-request, eight-thread batch, then distributions across batches; individual requests do not become independent samples. Transfer times exclude subsequent digest validation; job/worker times remain separate. The publisher exports same-directory summary, paired-comparison and provenance CSVs, with no P99 or public-network/model-latency claim. Failures prevent a complete comparison from being published; keep the failed report rather than replacing its cells with an older cohort.

B-APPLY's `product_v1.py --suites concurrent-conflicts` is a separate correctness probe. With at least 1,000 changed files (`--crash-files`, default 10,000), it observes a real target write, stops only its owned apply process, confirms a Prepared ledger and untouched file, injects/fsyncs an external host edit, then resumes the same apply. It requires the edited bytes to survive and a conflict exit, retaining partial-apply counts and final ledger state. A missed window is untested; silently overwritten content is a failed result. This is distinct from editing before apply and from SIGKILL recovery; none can substitute for the other. Run after formal performance timing, with three fresh repetitions, and retain every failed probe.

`publish_apply.py --report benchmark/.data/apply-new/report.json --output docs/src/en/benchmarks` exports the derived apply, recovery, comparison and provenance CSVs. It checks the retained binary/build receipt, source manifest, exact harness inventory, every planned operation/trial or known failure, and each requested versus durable crash state. Missing conditions are rejected; failed operations contribute no synthetic latency. Recovery times include only actual requested-state hits, while missed windows remain counted. At fewer than 30 pairs, Git comparison remains descriptive; P95 is absent below 30 samples and no P99 is generated. Conflict-before-apply and mid-apply external writes retain separate evidence. Validate the publisher before replacing both locale tables; complete reports and retained artifacts stay under `.data/`.

`publish_replay.py --report benchmark/.data/replay-new/report.json --output docs/src/en/benchmarks` requires all seven adapters, twenty native-format fixtures and three repetitions (420 conditions). The replay suite has no warmups. It rechecks the frozen harness, binary/source build receipt, seeded command order, exact arguments and historical observations, exclusion of future actions, zero tool execution and the unchanged pre-existing workspace. Failed conditions remain counted and contribute no successful latency. Review both locales before replacing the public `replay-fidelity.csv` (from derived `replay.csv`) and `replay-provenance.csv`; retain all raw artifacts under `.data/`. Separated timing clusters replace a single P50; P95 is descriptive and no P99 is generated. These synthetic-format checks do not execute installed Agent SDKs or compare native resume latency.

`publish_isolation.py --report benchmark/.data/isolation-new/report.json --output docs/src/en/benchmarks` requires all seven configurations and three fresh repetitions, with zero failures and unchanged prepared inputs. It verifies the retained harness, build/tool/firmware identities, seeded commands, independent Run Bundles, inside-view positives, outside accesses and exact final host/stage bytes. The derived `isolation-tests.csv` reports observed occurrence counts; it does not rank refusal latency. Review both locales before publishing it with `isolation-provenance.csv`. Original evidence remains in `.data/`; these fixtures do not establish comprehensive escape resistance.

`publish_supervision.py --report benchmark/.data/supervision-new/report.json --output docs/src/en/benchmarks` requires at least thirty paired rounds and three warmups for prepared stage/Git worktree review. It audits every retained command, full and selected diff, final twenty-file target and independent staged Run, including warmup evidence. The runner retains final target workspaces and records Git's executable digest/version before sampling. Exports are `supervision-summary.csv`, `supervision-comparisons.csv` and `supervision-provenance.csv`; review both locales before publishing. Their timer excludes task-view creation and execution, so keep them separate from B-WORKFLOW's complete-task CSVs. Raw evidence stays under `.data/`; human reading time is unmeasured.
