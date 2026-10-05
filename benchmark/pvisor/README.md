# pVisor benchmark runners

Read [the benchmark registry and writing rules](../README.md) before measuring or publishing. User questions, controls and roles belong to that registry; this manual owns commands and retention. Entry scripts carry `Benchmark:` declarations. Preparation, publication and plotting helpers serve their caller’s ID.

## Data and publication

Keep raw reports, per-trial samples, stdout/stderr, failures, input hashes, binaries and frozen harnesses in `.data/`. The repository ignores that directory at every depth. A new output directory is required for every run; retain slow valid samples and failed preflights. Do not build, run other tests or sample unrelated workloads during measurement.

Public Markdown contains derived comparisons. Supporting CSVs sit beside each article in `docs/src/{zh,en}/benchmarks/`, with download links. They identify cohort, resource budget, sample count, statistic and source digest. Full typed TSV reports, sample CSVs and archives stay local. Existing evidence is preserved in `docs/src/assets/benchmarks/.data/`; the verified migration inventory is in `benchmark/.data/raw-migration.json`.

`publication.py` publishes derived CSVs from a complete reference-runtime report. It rejects incorrect, duplicate and incomplete samples, keeps failures separate, never pools cohorts, omits P99 and omits P95 below 30 samples. P95 remains descriptive, not a stable tail-latency guarantee. A separated distribution replaces a single P50 with both cluster counts and medians: each cluster must contain at least `max(5, ceil(N*0.1))` samples, the largest eligible gap must be at least 20% of the overall median and more than three times the median adjacent gap, and cluster medians must differ by at least 1.5×. This is a descriptive rule, not a diagnosis.

```bash
python3 benchmark/pvisor/publication.py \
  --report benchmark/.data/reference-new/report.json \
  --output docs/src/zh/benchmarks
just test-benchmark
```

CSV summaries are not substitutes for raw evidence: a fresh checkout can read tables and inspect derived provenance, but rerunning requires prepared tools, firmware and local raw inputs. Do not create public links to ignored `.data/` paths. Site builds explicitly exclude them.

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
  --modes ready,filesystem,tools --samples 60 --warmups 3 --seed 20261005
```

Run `--samples 1 --warmups 0` into a separate new directory first. Every group requires valid outputs, declared isolation and complete staged writes; original host inputs must remain unchanged for staged jobs. `ready` measures first output, `filesystem` runs seven checked operations, and `tools` performs the fixed repair/test/diff plan. Each row records its benchmark ID. `claude,codex` are separate real-CLI workloads with deterministic local responses, not real inference or default nested-sandbox rankings. Failures make the runner exit nonzero after unaffected cases finish.

Tool preparation needs Linux x86_64, KVM, FUSE, user namespaces, a GNU pVisor CLI, firmware, Docker/Firecracker/QEMU, the Rust musl target, Git/rg/Python/Node/GCC and e2fsprogs. CLI modes also need their installed clients. Input copying, kernel builds, image import and downloads are outside timers. First output, result return and process exit are separate metrics. Record binary/source/harness digests, host kernel and tool identities with every run.

## Complete Ubuntu controls

`prepare_ubuntu_reference.py` prepares an official cloud disk with verified checksums, a generic kernel/initrd and tools. `ubuntu_baselines.py` checks systemd, networking, cloud-init, SSH readiness and workload correctness. It compares image-free pVisor with `firecracker-ubuntu`, `qemu-ubuntu` and `qemu-microvm-ubuntu`; first cloud-init boot is a separate case. This answers deployment waiting, not VMM overhead.

```bash
python3 benchmark/pvisor/prepare_ubuntu_reference.py --help
python3 benchmark/pvisor/ubuntu_baselines.py --help
python3 benchmark/pvisor/render_ubuntu_baselines.py --help
```

Use a frozen binary, new `.data/` outputs, matching CPU/tool-VM budgets and independent cohorts. Do not turn a failed preflight into a zero-latency sample or pool template/configuration changes.

## Task, resource and correctness suites

`product_v1.py` and its active `v1/` modules serve B-APPLY, B-NETWORK, B-DENSITY, B-ISOLATION, B-SUPERVISION and B-REPLAY. The versioned module name is a report/runner contract, not a retired product feature. It pins executable inputs, validates Bundle isolation and preserves failures. Apply uses Git patches as a control; network uses native and host-network Podman; density records attempted/completed counts as well as occupancy. Unmeasured Git-review and full-rollout comparisons must stay unmeasured in user pages.

```bash
python3 benchmark/pvisor/product_v1.py \
  --binary /absolute/path/to/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --replay-binary /absolute/path/to/pvisor-replay \
  --output benchmark/pvisor/.data/tasks-new \
  --samples 30 --warmups 3 \
  --suites network,apply,density,isolation,replay,supervision
```

Use `--help` for suite-specific counts and guards. Large apply jobs are capped at 3 samples, medium apply at 10, density at 5 batches and replay at 3 independent repetitions. Those do not establish tail latency. A guard is not a failure or a completed job. SIGKILL probes must hit the requested state; missed windows are retained separately. Snapshot/standalone CLI stress scripts are removed because that product entry is retired; use `just test-service-vm` for the current environment-sharing correctness gate.

B-CLUSTER uses `cluster_scalability.py`; derive figures with `plot_cluster_scalability.py --data-dir <local-.data-directory> --output-dir <public-derived-directory>`. Count validated task completions for throughput; VM-ready rate is only startup data. Increasing total CPU budget does not measure fixed-budget scaling.

## macOS

B-STARTUP uses `vm_ready.py`; B-MACOS uses `macos_docker_tools.py` for Docker Desktop and pVisor tool comparisons. B-VM-MEMORY uses `macos_cold_ram.py` with data-integrity checks. `macos_migration.py` is a separate engineering A/B runner. Keep Apple Silicon/HVF data separate from Linux/KVM. Cold guest residency and footprint do not establish net physical savings. Do not reuse the deleted standalone snapshot harnesses with current binaries.

## Engineering and diagnostics

B-FS-ENG runners remain active: `filesystem_ab.py`, `filesystem_fuse_ab.py`, `filesystem_stage_ab.py`, `filesystem_stage_durability.py`, `filesystem_lazy_ab.py`, `filesystem_kernel_probe.py` and `filesystem_diagnostic.py`. The FUSE passthrough adapter is a diagnostic control without staging semantics, not a production mode. Record each engineering run’s ID and keep raw output in `.data/`; publish only when it changes a user conclusion, after a matching user-facing comparison.

```bash
python3 benchmark/pvisor/filesystem_ab.py \
  --assets benchmark/.data/tools-new \
  --baseline /absolute/path/to/before/pvisor \
  --candidate /absolute/path/to/after/pvisor \
  --firmware /absolute/path/to/libkrunfw-directory \
  --output benchmark/pvisor/.data/filesystem-ab-new \
  --cpu-affinity 0,1 --samples 30 --warmups 3
```

Build frozen sources with only the proposed change between versions; report paired median differences with bootstrap confidence intervals. Instrumented profile runs remain separate from performance samples. `firmware_boot.py` and `guest_init.py` are startup diagnostics. `evidence_tsv.py` can read/write legacy raw formats locally; its format does not make a report suitable for public download.

## CI regression gate

B-PROCESS uses `bench.py` through `run.sh`. It checks a successful minimal Run and Bundle access, with 2 warmups/10 samples for smoke or 10 warmups/50 samples for nightly. It does not establish cross-runtime user rankings.

```bash
just benchmark
just benchmark nightly benchmark/pvisor/.data/nightly
just benchmark-compare \
  benchmark/pvisor/.data/candidate/raw-report.json \
  benchmark/pvisor/.data/main/raw-report.json
```
