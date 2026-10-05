# Agent tool tasks and CLI compatibility

## Main conclusions {#conclusions}

**pVisor staged short tool tasks are near native and beat Docker in the measured complete repair; VM tool execution is slower than Docker and minimal reference VMs.** Same-tool repair/test P50 is staged **0.70 s**, native **0.50 s**, Docker **0.90 s**, VM **3.97 s**, and QEMU microvm **1.85 s**. Staging/review adds hundreds of milliseconds here.

Image-free VMs return short tasks sooner than new complete Ubuntu environments, but their tools run more slowly once started. **Controlled Codex loops pass; Claude initialization times out in pVisor VM.** Client compatibility also matters.

## Motivation {#motivation}

Bare startup time does not describe an Agent editing files, installing dependencies and testing. Fixed tool plans and model responses isolate environment overhead and CLI functionality before considering model quality and variance.

## Experiment design {#interpretation}

Real Claude Code 2.1.128 / Codex CLI 0.160.0 use local controlled responses and fake credentials, without inference. Tasks inspect/search a repository, repair Python, run Python/Rust/Node tests, install 32 offline npm packages and generate a diff. Actual passing tests must return to the model service; clients must finish and staged originals remain unchanged.

Linux same-tool cells have three warmups and 30 samples; complete Ubuntu cells have N=10 and three warmups. Both use two cores, 2 vCPU / 16 GiB VMs and warm host caches. Complete Ubuntu changes tool versions and storage paths; tables remain separate. Task timing ends at verified results and excludes preparation; worker covers internal tools/checks. Codex uses inner `danger-full-access`, without validating default nested sandboxes. Matching macOS and real-model tasks are unmeasured.

## Data and analysis {#results}

### Same-tool environments: native, Docker and minimal VMs {#reference-env}

| Backend | Tool self-check P50/P95 s | Repair/tests P50/P95 s | Claude loop P50/P95 s | Codex loop P50/P95 s |
|---|---|---|---|---|
| Native | 0.15 / 0.17 | 0.50 / 0.78 | 0.82 / 0.88 | 1.97 / 2.49 |
| pVisor host | 0.16 / 0.17 | 0.50 / 0.61 | 0.85 / 0.91 | 1.94 / 2.25 |
| pVisor staged | 0.17 / 0.19 | 0.70 / 1.10 | 1.07 / 1.27 | 2.25 / 2.44 |
| pVisor VM | 1.40 / 1.52 | 3.97 / 6.79 | FAILED / N=0 | 10.93 / 12.93 |
| Docker rootless | 0.46 / 0.53 | 0.90 / 1.45 | 1.23 / 1.33 | 6.26 / 6.59 |
| Firecracker PCI | 1.48 / 1.59 | 2.25 / 3.13 | 3.03 / 3.21 | 7.83 / 8.47 |
| QEMU q35 | 0.92 / 1.09 | 1.98 / 3.11 | 2.67 / 3.67 | 7.69 / 8.46 |
| QEMU microvm | 0.85 / 1.07 | 1.85 / 2.48 | 2.71 / 3.29 | 7.67 / 8.34 |

Staged repair finishes about **0.20 s** before Docker, with different change/isolation semantics from Docker's writable bind. VM repair is about **4.4×** Docker; npm installation is a major waiting stage at **1.74 s**. Fast startup does not eliminate tool-path overhead.

Claude passes 30/30 on native, staged, Docker and all three reference VMs, but pVisor VM initialization exceeds 90 s, formal N=0. Codex passes 30/30 across all eight groups. Absolute timings across CLIs do not rank model speed; controlled responses do not establish unchanged real-model success rates.

### Image-free VMs and complete Ubuntu {#full-ubuntu}

| Backend | Tool self-check P50/P95 s | Repair/tests P50/P95 s | Claude loop P50/P95 s | Codex loop P50/P95 s |
|---|---|---|---|---|
| Native / Fedora | 0.16 / 0.17 | 0.52 / 0.55 | 0.92 / 1.07 | 2.03 / 3.12 |
| pVisor staged | 0.18 / 0.19 | 0.72 / 0.78 | 1.15 / 1.58 | 2.33 / 2.44 |
| pVisor VM / host | 1.21 / 1.77 | 4.61 / 5.09 | FAILED / N=0 | 11.39 / 13.73 |
| Firecracker / Ubuntu | 7.79 / 10.13 | 8.51 / 9.11 | 9.78 / 9.87 | 10.81 / 13.49 |
| QEMU q35 / Ubuntu | — | 8.12 / 8.23 | 9.50 / 10.78 | 10.20 / 11.63 |
| QEMU microvm / Ubuntu | — | 10.33 / 10.47 | 11.59 / 14.07 | 13.04 / 14.06 |

pVisor VM repair takes **4.61 s** from launch to result versus **8.51 s** for Firecracker/Ubuntu; internal tools alone take **4.02/2.30 s**. Reduced boot waiting helps disposable short tasks; reused environments depend more on tool speed. QEMU rows are separate N=10 cohorts using the same Ubuntu template, without pooled distributions. Reports retain phase timing and memory scope.

### Scope {#acceptance}

These comparisons use their pinned pVisor artifacts and have not all been rerun alongside current filesystem artifacts. Large repositories, real inference, Internet dependencies, persistent-pool throughput and SWE-bench success rates are unmeasured. [Current filesystem results](filesystem.md) provide the latest local operation timings.

### Data sources and reproduction {#run}

[Same-tool distributions](../../assets/benchmarks/reference-env-20261004/summary.tsv) · [Compatibility](../../assets/benchmarks/reference-env-20261004/compatibility.tsv) · [Ubuntu](../../assets/benchmarks/full-ubuntu-20261004/summary.tsv) · [QEMU / Ubuntu](../../assets/benchmarks/full-ubuntu-qemu-20261004/summary.tsv) · [Methodology](methodology.md) · [Phase analysis and reproduction](../design/agent-task-performance-analysis.md)
