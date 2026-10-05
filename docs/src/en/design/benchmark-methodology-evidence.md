# Benchmark measurement protocols and evidence

All benchmarks follow the same protocol:

- State the hardware, operating system, kernel or macOS version, FUSE implementation, pVisor version, and commit.
- Report p50, p95, and p99 with the sample count.
- Provide a script that reproduces the result in one command (under `benchmark/`), reporting through the `pvisor-benchmark/v1` schema.
- State the configuration of every control group; do not compare against untuned competitors.
- Keep results by date instead of overwriting old data.

## Retaining macOS and Linux data

Integrate and retain both macOS/HVF and Linux/KVM datasets in the corresponding benchmark articles, listed by host OS, architecture and backend in the [benchmark overview](../benchmarks/index.md). Every run records its date, hardware, configuration, source state, executable and firmware hashes, and raw samples. Reruns use new directories and retain earlier reports, charts and JSON/CSV files. Summary pages may reference a newer batch while keeping links to earlier batches.

Calculate distributions separately for each platform. Cross-platform comparisons require matching workloads, timing boundaries, cache states and parameters. Label unmeasured areas and state platform restrictions for unsupported features. Interpret Linux offload and macOS shared cold-page reclamation as separate datasets.

Available data: [macOS startup latency](../benchmarks/startup.md), [macOS shared cold-page reclamation](../benchmarks/vm-memory/index.md), and [Linux lifecycle and complete snapshots](vm-memory-performance-analysis.md#linux-methodology).

## Environment checklist

Start every report with this table:

| Item | Example |
| --- | --- |
| Date | 2026-10-02 |
| pVisor version and commit | `0.x.y` / `abc1234` |
| Hardware | Apple M4, 16 GiB; or CPU model, core count, memory |
| OS and kernel | macOS 26.x; or Ubuntu 24.04, Linux 6.8 |
| Filesystem and FUSE implementation | APFS + macFUSE 5.x; or ext4 + libfuse 3.x |
| Executor and options | `--executor vm --overlaynet auto` |
| Samples and warmups | 100 runs, discarding the first 5 |

## Control groups

- Use each alternative's documented recommended configuration, and state its version and options.
- Isolate pVisor's own overhead: for example, the difference between `--filesystem host` and staging under the same executor, rather than only the end-to-end total.
- When you have data for only part of a phase (for example, only guest init), the title must name the phase being measured; do not present it as an end-to-end conclusion.

## Start from the existing measurement entry points

```bash
just benchmark
just benchmark nightly target/pvisor-benchmark/nightly
just benchmark-compare target/pvisor-benchmark/candidate/raw-report.json target/pvisor-benchmark/main/raw-report.json
# Linux：启动与资源占用矩阵
just benchmark-startup --warmups 10 --samples 100
```

`just benchmark` measures the process-level cost of a minimal host Run and of reading its Run Bundle: smoke uses 2 warmups and 10 samples, nightly uses 10 warmups and 50 samples. Both use `pvisor-benchmark/v1`. `benchmark-startup` writes separate `startup.json`/`startup.md` files and does not pretend to share the same report schema; its default is 3 warmups and 30 samples, overridden explicitly above.

Building, image downloads, and rootfs preparation are not counted in the existing startup samples; reports must state this boundary. Executors that fail preflight are listed separately as SKIP with stderr retained. A successful measurement requires the command to succeed and the Bundle to be completed with a zero exit. Failures, skips, or degraded controls cannot count as faster successful samples.

## Interpretation and archiving

Compare candidate and baseline on the same host, suite, and inputs. `benchmark-compare` defaults to a 15% regression threshold and reports only unless `--fail-on-regression` is explicitly enabled. Save raw samples, summaries, complete parameters, input hashes, and commits; distinguish cold images, warm disk caches, and warm page caches, and record background load and power state.

The first product measurements are recorded below; real-model success, human supervision and cloud timings remain unmeasured.

`benchmark/pvisor/vm_ready.py` uses `pvisor-vm-readiness/v1`, measuring workload readiness and CLI completion separately, with independent host-checkpoint diagnostics. See [startup latency](../benchmarks/startup.md) for the protocol.

## Product benchmark first version, 2026-10-04 {#product-v1}

Host tool time is close to native. Staging adds tens of milliseconds for sequential reads/offline npm and about 150–190 ms for small-file operations. Most measured small VM jobs take 0.5–1 second. Applying 10/1,000/100,000 files costs roughly 15 ms/0.84 seconds/5.5 minutes. These describe this configuration, not all repositories.

### Environment and identity

| Item | Recorded configuration |
|---|---|
| Date/platform | 2026-10-04, Linux x86_64 |
| CPU/RAM | Ryzen 7 9700X, 8 cores/16 threads; MemTotal 31,980,420 KiB (about 30.5 GiB) |
| OS | Fedora 44, Linux 7.2.8-200.fc44.x86_64 |
| Filesystems | home btrfs, Linux FUSE; default /tmp is 16 GiB tmpfs with user quota |
| pVisor | 0.3.0; initial worktree based on 8c63f4f0, with parallel development/dirty changes preserved in source_status |
| CLI SHA256 | `592b01a0683b8eeedc4750d04b7400820034fa84e630c0dcecc9eb742b800383` |
| libkrunfw | 5.5.0, SHA256 `6df51f65d7f99fc22215e69a4236c770b1588ceb6777eca014f92b366517d237` |
| Repaired replay SHA256 | `a69f7a6e40e4bbc71fb1e5af6c957fd7c59c194c3e719c4e497c492044b33f32` |
| Container/Agent | rootless Podman/crun 1.28, Claude 2.1.128, Codex 0.160.0; reports retain image IDs and versions |
| Background work | Shared desktop, editors and parallel development; not a dedicated idle benchmark host. Reports retain before/after load; supplementary batches overlapped one large apply |

### Controls

| Name | Configuration and scope |
|---|---|
| native | Direct execution of identical input/tool |
| host | Host process, OverlayNet off; host paths remain accessible |
| staged | Host plus staged workspace; outside paths are not confined by staging alone |
| safe | Rootless filesystem sandbox, stage and proxy; read-only Rust toolchain share |
| vm | libkrun/KVM, 2 vCPU; main tools use 1 GiB and host rootfs `/` plus stage; isolation uses a prepared rootfs; idle probe uses 128 MiB |
| podman | Prepared local OCI image, workspace bind mount, crun; network none for tools, host network for networking |
| container | pVisor OCI/crun, writable workspace mount, network none/host; wall includes per-Job private rootfs copying |

These groups have different boundaries. A successful pVisor sample requires a completed zero-exit Bundle and the requested observed executor; staged/safe/VM also require observed staging. File data, tool output or network hashes must validate before entering rows.

### Sampling

Warm host caches, no eviction. Image construction/import and input preparation are outside timed tasks; cargo's workload compilation is inside tool time. Filesystem/network use 3 warmups/30 samples with randomized backend order. Apply uses N=30/10/3 for 10/1,000/100,000 files, one warmup for smaller cases and none for the largest. Density has five batches per cell, agent/replay three repetitions per task, supervision thirty; these have no warmups. Quantiles interpolate linearly. Nested requests/jobs may correlate; no independent-sample confidence intervals are claimed.

Wall covers command launch through exit. Worker covers internal tool execution/validation. Failed batches retain attempted/completed counts and diagnostics. Memory guards are not successes or zero-cost results. An inaccessible Docker daemon, missing image tools and Node address-space failures are explained separately.

### Raw evidence

[Batch manifest](../../assets/benchmarks/product-v1-20261004/manifest.tsv), [per-sample CSV](../../assets/benchmarks/product-v1-20261004/samples.csv), and [evidence archive](../../assets/benchmarks/product-v1-20261004/evidence.tar.gz) retain JSON, exact harness snapshots, Bundles, errors and SIGKILL ledgers. Rootfs/payloads are reproducible and excluded from docs to avoid gigabytes of copies. Prior macOS/Linux VM results remain intact. New product workloads are Linux-only and are not pooled across platforms.

`just lint` passed. `just test`: Rust 1,075 passed / 8 skipped, Python 84 passed / 16 skipped. Benchmark tests alone: 19 passed. STAGE semantics: 14/14 PASS, with human review still UNREVIEWED; test success is not human approval. Large syscall traces and non-crash apply ledgers remain in local target artifacts; the public archive retains reports, Bundles, diagnostics and crash ledgers.

Recompute with `python3 benchmark/pvisor/summarize_product_v1.py <batch>/report.json --output-csv /tmp/summary.csv`; [summary CSV](../../assets/benchmarks/product-v1-20261004/summary.csv) keeps batches separate.

Performance describes pinned artifacts, not later parallel changes or other release builds; source was not a clean commit. Workspace lint/tests validate the then-current source, while the pinned CLI is checked by benchmarks and STAGE specifications.

## Complete Ubuntu and image-free pVisor: deployment protocol {#full-ubuntu}

This batch adds the practical cost of a standard distribution environment. Firecracker uses the complete official Ubuntu cloud VM disk, generic distribution kernel and initrd. pVisor VM always uses directories; this batch uses `--rootfs host` to reuse installed host tools. Latest Ubuntu is resolved and pinned to Ubuntu 26.04.1 LTS, release 20260927, from the [official dated snapshot](https://cloud-images.ubuntu.com/releases/resolute/release-20260927/). It is a cloud VM image, rather than a full system assembled from Docker OCI layers.

Ubuntu retains GPT, /boot, EFI partitions, vendor modules, fstab, systemd, cloud-init, SSH, snapd and other services. Only NoCloud static networking, a benchmark unit and Agent applications are added. Stock `7.0.0-34-generic` supports ext4, FUSE and btrfs with `CONFIG_EXT4_FS=y`, `CONFIG_FUSE_FS=y`, and `CONFIG_BTRFS_FS=m`. Firecracker 1.13.1 loads ELF directly, so the official bzImage is decompressed unchanged, without trimming or recompilation. It uses the stock initrd and default virtio MMIO. Boot partitions remain on disk; this path does not execute UEFI/GRUB.

First boot uses a complete template that has not run cloud-init; prepared boot uses a disk template after normal tool installation and initial setup. Each trial clones a private disk and creates a fresh VM without restoring RAM. Launch-to-first-output after full OS readiness, validated task result, and normal zero exit are separate measurements. Ubuntu must prove PID1 is systemd and multi-user/network-online/cloud-final/ssh.socket are active, retaining the failed-unit list every time. No formal sample has failed units. Pre-launch workspace copying/disk cloning is recorded as `prepare_ms`, outside startup and task time.

The host matches the previous controls: Fedora 44, Ryzen 7 9700X, KVM, with frozen CLI SHA256 `1a2db5ad015c5ace40b3c96c7dd0dc94a08b8893de35150bd909554286e2cd3c`. All launcher trees use physical host cores 0 and 1. VMs have 2 vCPU, 2 GiB for startup and 16 GiB for complete tasks; native/staged have no hard memory cap. Host page caches are warm and backend order is randomized per round. Startup uses N=30 per case; published tools/files/clients cells use N=10, with Claude/VM preflight failure at N=0 and 3 warmups per available case. N=10 is exploratory first-version evidence; P95/P99 cannot establish a tail-latency guarantee. The shared desktop retains background activity, with before/after load recorded.

Complete tool tasks reuse the same project, inputs and grading. Rust/Cargo 1.98.1, Claude 2.1.128 and Codex 0.160.0 use the same artifacts. Ubuntu apt provides Python 3.14.4, Node 22.22.1 and Git 2.53.0, differing from host Python 3.14.7, Node 24.18.0 and Git 2.55.0. This compares normal deployment stacks; task differences cannot all be attributed to filesystems or VMMs. CLIs connect to a fixture model service in the same environment and must return actual passing tool results. There is no real inference, paid account, token or Internet-model timing. Codex uniformly uses inner `danger-full-access`; default nested sandbox compatibility is unmeasured.

Firecracker has a TAP and static NIC in private user/network namespaces. A QEMU `-machine none` process supplies user-mode DNS/NAT so Ubuntu default services have working connectivity. There is no second guest VM; helper startup, CPU and RSS are included in the Firecracker group. Host routes and network services are unchanged. Preflight found that dead DNS caused a 30-second snapd wait; formal batches use functionally verified DNS/NAT. Earlier PCI trials hung in device reset during normal reboot and contribute N=0. Default MMIO exits normally with zero status. Firecracker runs without jailer; production security deployment is not assessed.

The successful provisioning step boots QEMU, copies installed Rust/CLI/fixtures, and apt-installs distribution tools in **85.8 seconds**, recorded separately. It excludes the 825 MiB official qcow2 download, format conversion, tool payload creation, host prerequisite installation and failed diagnostic attempts. It is not total deployment cost from an empty machine. pVisor needs no OS image preparation, while still requiring installed host tools.

Image/kernel/initrd hashes and sources are in the [asset manifest](../../assets/benchmarks/full-ubuntu-20261004/assets.tsv). Official HTTPS SHA256SUMS are checked; GPG signatures are not verified. The full [vendor kernel configuration](../../assets/benchmarks/full-ubuntu-20261004/ubuntu-generic.config) and per-trial proofs are available. The separate archive uses `pvisor-full-ubuntu-reference/v1`, retaining harness snapshots, failures, commands, OS proofs and minimized runtime evidence without pooling with the trimmed-kernel batch. Initial Ubuntu client sampling missed endpoints when the serial prompt/terminal control sequences shared a result line. After fixing the collector, Claude/Codex are each repeated at N=10 with identical parameters. Published client cells use only the follow-up distributions without pooling old samples. The original main report and invalid trials remain available; the [archive manifest](../../assets/benchmarks/full-ubuntu-20261004/manifest.tsv) records the selected batch per case.

### Reproduce the complete Ubuntu comparison

Requires Linux x86_64, KVM, FUSE, user/network namespaces, Firecracker 1.13.1, QEMU, curl, zstd, e2fsprogs, sfdisk, and installed host Python/Node/Rust/GCC/Git/rg/Claude/Codex. Use fresh output directories; downloads and tool installation require networking. Preparation modifies regular files under its output directory, without changing host services or block devices. Preflight with `--samples 1 --warmups 0` before formal runs. The fixture contains only project/workload inputs, not an OS root directory.

```bash
python3 benchmark/pvisor/prepare_ubuntu_reference.py \
  --output target/ubuntu-reference-new
python3 benchmark/pvisor/ubuntu_baselines.py \
  --assets target/ubuntu-reference-new --fixture target/ubuntu-reference-new/fixture \
  --binary /absolute/path/to/frozen/pvisor --output target/ubuntu-ready-new \
  --backends native,pvisor-staged,pvisor-vm-hostroot,firecracker-ubuntu,firecracker-ubuntu-firstboot \
  --modes ready --samples 30 --warmups 3
python3 benchmark/pvisor/ubuntu_baselines.py \
  --assets target/ubuntu-reference-new --fixture target/ubuntu-reference-new/fixture \
  --binary /absolute/path/to/frozen/pvisor --output target/ubuntu-workflows-new \
  --backends native,pvisor-staged,pvisor-vm-hostroot,firecracker-ubuntu \
  --modes env,filesystem,tools,claude,codex --samples 10 --warmups 3
python3 benchmark/pvisor/ubuntu_baselines.py \
  --assets target/ubuntu-reference-new --fixture target/ubuntu-reference-new/fixture \
  --binary /absolute/path/to/frozen/pvisor --output target/ubuntu-clients-new \
  --backends firecracker-ubuntu --modes claude,codex --samples 10 --warmups 3
uv run --no-project --with matplotlib python benchmark/pvisor/render_ubuntu_baselines.py \
  --report target/ubuntu-ready-new/report.json \
  --report target/ubuntu-workflows-new/report.json \
  --report target/ubuntu-clients-new/report.json \
  --replace-cohort claude/firecracker-ubuntu --replace-cohort codex/firecracker-ubuntu \
  --assets target/ubuntu-reference-new --output /tmp/full-ubuntu-report-new
```

[Per-sample CSV](../../assets/benchmarks/full-ubuntu-20261004/samples.csv) · [Distributions and phases](../../assets/benchmarks/full-ubuntu-20261004/summary.tsv) · [Method and reproduction](../benchmarks/methodology.md#full-ubuntu)

See the [dpkg package inventory](../../assets/benchmarks/full-ubuntu-20261004/ubuntu-packages.txt). Formal tasks uniformly place compiler/client temporary files in private workspace `_tmp` to accommodate hostroot read-only `/tmp`. Failed diagnostics are retained outside the new formal distributions.

The frozen binary [source limitations](../../assets/benchmarks/full-ubuntu-20261004/binary-provenance.tsv), [project input hashes](../../assets/benchmarks/full-ubuntu-20261004/fixture-inputs.tsv) and [temporary-directory diagnostics](../../assets/benchmarks/full-ubuntu-20261004/diagnostics/ubuntu-workflows-mmio-20261004/diagnostic-status.tsv) are retained separately. Combined CSV marks published samples with `selected_for_summary` to avoid pooling client follow-ups with previous cohorts.

### QEMU follow-up with complete Ubuntu {#full-ubuntu-qemu}

QEMU 10.2.2 q35 and microvm reuse the same complete Ubuntu Agent disk measured on Firecracker, stock `7.0.0-34-generic` and official initrd, loading the official bzImage directly; UEFI/GRUB are not executed. q35 uses virtio PCI; microvm uses virtio MMIO with optional legacy devices disabled and serial retained. The [official microvm documentation](https://www.qemu.org/docs/master/system/i386/microvm.html) describes direct boot and normal guest exit. Both explicitly select KVM, host CPU and 2 vCPU, with 2 GiB for startup and 16 GiB for tasks, bound to host cores 0 and 1.

Each QEMU process supplies built-in user-mode NAT/DNS; Firecracker needs a separate QEMU `-machine none` helper whose time and RSS are included. Differences therefore cannot all be assigned to VMM or block-device cost. Each trial clones the same template; cases have 3 warmups and N=10, plus a functional preflight. They require the same OS-service readiness, tool checks and normal zero exit as Firecracker. `reboot=t` and `-no-reboot` allow normal termination rather than counting a host kill as success.

The follow-up runs separately on the same host without pooling earlier samples; plotted pVisor/Firecracker distributions come from their previous batches. N=10 supplies an initial budget, not a tail guarantee. Commands, logs, OS proofs and artifact hashes are archived with the results. This command reuses the official Ubuntu prepared above:

```bash
python3 benchmark/pvisor/ubuntu_baselines.py \
  --assets target/ubuntu-reference-new --fixture target/ubuntu-reference-new/fixture \
  --binary /absolute/path/to/frozen/pvisor --output target/ubuntu-qemu-new \
  --backends qemu-ubuntu,qemu-microvm-ubuntu \
  --modes ready,tools,claude,codex --samples 10 --warmups 3
```


The follow-up [80 new samples and independent control cohorts](../../assets/benchmarks/full-ubuntu-qemu-20261004/manifest.tsv) all pass. The prerequisite for disabling PIC/PIT was checked by a [QMP capability query of the KVM host CPU model](../../assets/benchmarks/full-ubuntu-qemu-20261004/qemu-cpu-capabilities.tsv): the guest model exposes `tsc-deadline`. This is a capability check, not a performance sample.

## Familiar baselines and complete Agent Env: same-host Linux comparison {#reference-env}

This batch separates first-output latency, working tools, and complete client repair loops. Startup, file operations, complete tools and real CLI timing are distinct. Historical macOS/Linux data remain intact; the new Docker/Firecracker/QEMU matrix is Linux only.

| Item | Recorded value |
|---|---|
| Host | Fedora 44 / Linux 7.2.8-200.fc44.x86_64; Ryzen 7 9700X, 8C/16T; 30.5 GiB |
| Tools | Python 3.14.7; Node 24.18.0; Rust/Cargo 1.98.1; Claude 2.1.128; Codex 0.160.0 |
| References | Docker Engine 29.7.2 rootless; Firecracker 1.13.1 PCI; QEMU 10.2.2 q35 / microvm |
| pVisor SHA256 | `1a2db5ad015c5ace40b3c96c7dd0dc94a08b8893de35150bd909554286e2cd3c` |
| Guest kernel | Linux 6.12.109; reference kernel/config shared by Firecracker/QEMU |
| VM RAM / CPU | 128 MiB shell probe; 16 GiB complete environment; 2 vCPU |
| Storage | btrfs host; Docker bind mount; pVisor staged virtio-fs; reference private ext4 |
| Sampling | 30 samples + 3 warmups per available case; random order; hot host caches |
| Effective samples | 1,410 passed; 47/48 mode/backend configurations available |
| Identity/source | Frozen binary and harness hashes; parallel dirty development recorded, not a clean commit |


Docker/native have no hard memory cap. Payloads and VMMs use physical host cores 0 and 1; the private Docker daemon and in-container tools are explicitly pinned, with Rust `-j2`. The Docker daemon is already running; its one-time startup is outside task time. Run Bundles/VMM configs verify guest RAM; configured capacity and sampled RSS are separate. This is a shared desktop with development on other cores, shared cache/disk activity and unlocked frequency. Small percentage differences do not establish reliable rankings.

Both QEMU q35 and microvm are included. The microvm configuration disables optional legacy devices and uses KVM/host CPU and virtio-blk. Firecracker uses PCI, no API and no jailer. All three boot a trimmed kernel and static init directly, without systemd/SSH/cloud-init. Parameters follow [QEMU microvm documentation](https://www.qemu.org/docs/master/system/i386/microvm.html), [Firecracker 1.13.1](https://github.com/firecracker-microvm/firecracker/blob/v1.13.1/docs/getting-started.md), and [Docker rootless](https://docs.docker.com/engine/security/rootless/). This is a development performance baseline, not a production security assessment. Firecracker/QEMU are not pVisor executors.

pVisor's embedded firmware has the same kernel version but a different configuration. Its path also provides staging, execution observations and Run Bundles. Block/ext4 versus virtio-fs, client initialization and exit protocols differ; complete-command differences cannot be attributed solely to VMM overhead.

### Timing, correctness, and failures

`ready_ms` measures first output or completed tool self-check; `result_ms` measures a validated task result, both including runtime startup. `completion_ms` includes exit; `prepare_ms` separately records workspace/private-disk preparation. The main batch's exit polling can add roughly 50 ms of granularity. Primary conclusions use stdout event timing; a precise-exit follow-up remains a separate batch. File tables use internal `worker_ms`, excluding environment startup.

One capability preflight precedes 3 excluded warmups and 30 seeded, randomized rounds. Correctness requires zero exit, exactly one result, matching mode, actual successful grading, correct files and original state, and no VM panic. pVisor additionally requires completed Bundles, the requested actual executor, and staging observations. Failures retain logs and never enter latency distributions. Claude/VM initialization times out at 90 s, so formal N=0; the runner completes other cases then exits nonzero. Uniform Codex inner `danger-full-access` does not establish compatibility with its default inner sandbox.

Real Claude/Codex CLIs use a local deterministic response server in the same environment. No real inference, Internet latency, actual tokens or billing are measured. Thirty repetitions describe one task, not thirty independent defects. P95/P99 describe this batch, not long-term tails or success-rate confidence intervals.

### Resource audit {#reference-resources}

RSS is summed at 20 ms intervals, not PSS or a strict cgroup peak; short peaks and shared-page double counting are possible. The initial main sampler tracked Docker CLI/daemon descendants, but an audit found that systemd adopts container shims, excluding their tools. Published main data now label that partial scope. Its roughly 102 MiB cannot be ranked against complete VM trees. A separate batch follows the exact container ID/shim and task, retaining process ancestry and affinity evidence. Resource results are presented separately in that report.

#### Complete-tool resource follow-up

Same configuration, 10 samples per backend and 1 warmup, tracking Docker shims by exact container ID. Docker includes its dedicated daemon's fixed cost: about 75 MiB at batch start. It is not per-container incremental memory and cannot be divided into host RAM to predict Agent density. VM RSS includes the VMM and touched guest pages, not its 16 GiB configured capacity. Environment construction/image import did not overlap measurement.

| Backend | N | Peak RSS P50/P95 MiB |
|---|---|---|
| Native | 10 | 168.8 / 171.0 |
| pVisor host | 10 | 178.5 / 180.7 |
| pVisor staged | 10 | 226.8 / 242.5 |
| pVisor VM | 10 | 722.1 / 734.9 |
| Docker rootless | 10 | 280.3 / 320.5 |
| Firecracker PCI | 10 | 762.7 / 771.4 |
| QEMU q35 | 10 | 812.3 / 818.9 |
| QEMU microvm | 10 | 803.0 / 818.6 |


Native/staged use roughly 0.17/0.22 GiB; Docker including the daemon about 0.27 GiB; the VM about 0.71 GiB. pVisor VM and reference VMs are in the same sub-1-GiB range for this task. This short workflow does not cover large-repository long-run peaks or shared-page savings. Different scopes do not establish a strict physical-memory efficiency ranking.

[Resource report](../../assets/benchmarks/reference-env-20261004/followups/reference-resources-20261004/report.tsv) · [Samples](../../assets/benchmarks/reference-env-20261004/followups/reference-resources-20261004/samples.csv) · [Process/affinity audit](../../assets/benchmarks/reference-env-20261004/followups/reference-resources-20261004/docker-process-audit.tsv) · [Evidence](../../assets/benchmarks/reference-env-20261004/followups/reference-resources-20261004/evidence.tar.gz)


### Deployment and reproduction

Build a rootfs from installed tools, import the matching Docker image and create ext4. Automatic Fedora 44 layout supports Python 3.14/Node 24; another system can supply an equivalent prepared `--tools-rootfs`. Prerequisites are KVM, FUSE, private rootless Docker, Firecracker, QEMU, Claude/Codex, the Rust musl target, GCC/e2fsprogs and clean Linux 6.12.109 sources. Tools and fake credentials are copied, not login authentication.

Start a user-owned private daemon following the rootless Docker instructions, with a short socket path. Replace `12345` below with its actual host PID and select two allowed physical cores. Owner/socket checks precede pinning only that daemon; system Docker is untouched. Output directories must be new. Start with `--samples 1 --warmups 0`. Downloads, kernel compilation and image preparation are outside task timing; offline preparation and per-run cloning are recorded separately.

```bash
bash benchmark/pvisor/prepare_reference_kernel.sh \
  /absolute/path/to/clean/linux-6.12.109 target/reference-kernel-new
python3 benchmark/pvisor/prepare_reference_env.py \
  --binary /absolute/path/to/pinned/pvisor --output target/reference-env-new \
  --kernel-elf target/reference-kernel-new/vmlinux \
  --kernel-bzimage target/reference-kernel-new/arch/x86/boot/bzImage \
  --kernel-config target/reference-kernel-new/.config \
  --docker-host unix:///tmp/pvisor-reference-docker/docker.sock
python3 benchmark/pvisor/reference_baselines.py \
  --assets target/reference-env-new --binary /absolute/path/to/pinned/pvisor \
  --output target/reference-results-new \
  --docker-host unix:///tmp/pvisor-reference-docker/docker.sock \
  --docker-root-pid 12345 --cpu-affinity 0,1 --memory-mib 16384 \
  --samples 30 --warmups 3
uv run --no-project --with matplotlib python benchmark/pvisor/render_reference_baselines.py \
  --report target/reference-results-new/report.json \
  --assets target/reference-env-new --output /tmp/reference-report-new
```

The archive uses `pvisor-reference-environment/v1`, distinct from the old smoke schema. It includes samples, commands, pinned scripts, guest/tool versions, failures, minimized real tool-return evidence, and Bundle isolation/resource proof, excluding multi-GB rootfs and inherited host credentials. Reports contain tool/kernel/pVisor identities. See the [benchmark directory](https://github.com/DeepLink-org/pvisor/tree/main/benchmark/pvisor) for automatic preparation and reproduction.

[Per-sample CSV](../../assets/benchmarks/reference-env-20261004/samples.csv) · [Distributions and phase timing](../../assets/benchmarks/reference-env-20261004/summary.tsv) · [Runtime evidence](../../assets/benchmarks/reference-env-20261004/evidence.tar.gz) · [Compatibility matrix](../../assets/benchmarks/reference-env-20261004/compatibility.tsv)
