# Benchmark methodology and comparison scope

## Main conclusions {#conclusions}

These results describe waiting, resources and reliability for specific tasks. **Direct comparisons require matching configurations and timing definitions.** Minimal VMs and complete Ubuntu, tool time and launch-to-exit, RSS and cgroup memory are reported separately. Vendor claims do not fill gaps for unmeasured tools.

## Motivation {#motivation}

Faster startup need not mean faster tasks; writable mounts and staged views offer different workflows. Public comparisons should make the measured environment, source of waiting and applicability clear.

## Experiment design {#interpretation}

### Environments and artifacts

| Evidence scope | Configuration and identity |
|---|---|
| Current local filesystem | Linux/KVM, two-core budget, 2 vCPU / 4 GiB VMs; frozen integrated source, separate release/performance measurements |
| Same-tool references | Linux/x86_64, Docker Engine 29.7.2 rootless, Firecracker 1.13.1, QEMU 10.2.2; 2 vCPU, shell 128 MiB, tool tasks 16 GiB |
| Complete-distribution deployment | pVisor host rootfs; reference Ubuntu 26.04.1 with generic kernel, initrd and systemd; startup 2 GiB, tools 16 GiB |
| macOS | Apple M4 / HVF; startup and cold-page results use their pinned report configurations |
| Cluster | 1/2/4 Workers with growing CPU budgets; frozen debug probes are separate from release startup |

Version numbers identify measured software, not the latest third-party releases. A source revision does not identify a dirty tree's executable; use binary SHA256, input digests, parameters and reports. Current filesystem reruns cover local and lazy paths. Other topics retain evidence for their pinned artifacts; the entire suite has not been rerun on the current integrated version.

### Timing and correctness

Ready runs from before host command launch to first useful guest output; worker covers internal operations and checks; task time ends at the result; Exit records process termination separately. Downloads, image builds, tool installation and per-trial input preparation are separate. The default is three warmups and 30 randomized samples; N=10, N=3 and sequential measurements are specified on each page.

Validation checks zero exit, file count/content, SHA256, build/test results, Run Bundles and actual executors. Failed cases do not enter successful latency distributions; counts, reasons and capacity guards remain recorded. Shared hosts do not fully isolate background activity, lock frequency or flush all caches. Small differences do not establish stable rankings.

### Comparison conditions

Docker uses an already-running private rootless daemon and writable bind mount. pVisor staged retains changes; standalone stage does not block outside-view access. VM virtio-fs uses the shared filesystem service. Firecracker/QEMU use private ext4. Reports pin kernels, networks, tool versions and security parameters; development baselines do not rank production security deployments.

### Resource definitions {#reference-resources}

RSS sums tracked processes every 20 ms; shared pages can be counted twice and short peaks missed. Docker tracking must include its container-ID shim, with a dedicated daemon explicitly included where applicable. Configured guest RAM differs from actual residency. Cluster sums nonoverlapping cgroup `memory.current`; file memory can include guest RAM and cannot simply be subtracted. The macOS cold-page RAM proxy is not net physical memory saved.

## Data and analysis {#results}

### Current filesystem evidence {#filesystem-service}

[Local release](../../assets/benchmarks/filesystem-service-20261005/local-release.tsv) · [Local performance](../../assets/benchmarks/filesystem-service-20261005/local-performance.tsv) · [Lazy cold/warm](../../assets/benchmarks/filesystem-service-20261005/lazy-performance.tsv) · [Artifact manifest](../../assets/benchmarks/filesystem-service-20261005/manifest.tsv)

Each local profile has 150 measured jobs, plus 120 lazy jobs: 420 jobs and 2,820 operation measurements. The cache fixture checks on-demand block reads and warm client caches; it does not measure the production Rust cache, network or S3. Version A/B interpretation stays in [technical analysis](../design/filesystem-performance-analysis.md#filesystem-service).

### Same-tool references {#reference-env}

Groups share tool artifacts and inputs. Reference VMs use trimmed kernels and static init, without booting a full distribution. First output, file operations and CLI loops support [startup](startup.md#reference-startup), [filesystem](filesystem.md#reference-fs) and [complete-task](agent-tasks.md#reference-env) comparisons.

[Summary and distributions](../../assets/benchmarks/reference-env-20261004/summary.tsv) · [Samples](../../assets/benchmarks/reference-env-20261004/samples.csv) · [Compatibility](../../assets/benchmarks/reference-env-20261004/compatibility.tsv)

### Complete Ubuntu deployment {#full-ubuntu}

The complete Ubuntu cloud VM uses its distribution kernel, initrd and normal services; pVisor reuses host tool directories. This measures user waiting across deployment approaches, without isolating VMM performance. First cloud-init and configured templates are separate; downloads and template preparation are excluded.

[Summary](../../assets/benchmarks/full-ubuntu-20261004/summary.tsv) · [Samples](../../assets/benchmarks/full-ubuntu-20261004/samples.csv)

### Complete-distribution QEMU configuration {#full-ubuntu-qemu}

q35 and microvm share a complete Ubuntu template, N=10 per cell. Their distributions remain separate from Firecracker/pVisor, without pooling samples.

[Summary](../../assets/benchmarks/full-ubuntu-qemu-20261004/summary.tsv) · [Configuration manifest](../../assets/benchmarks/full-ubuntu-qemu-20261004/manifest.tsv)

### Other topics and reproduction {#product-v1}

Network, apply/drop, isolation, replay, supervision and density parameters and samples are linked from each topic and the [raw manifest](../../assets/benchmarks/product-v1-20261004/manifest.tsv). [Technical methodology evidence](../design/benchmark-methodology-evidence.md) retains detailed environments, commands, failure diagnostics and resource audits. Reruns use new output directories, pin inputs/artifacts, retain failures and keep distributions from different environments separate.

## Benchmark evidence TSV {#evidence-format}

Published reports in `docs/src/assets/benchmarks/` use three TSV columns: `path`, `type`, and `value`. Each field occupies one line, so a changed statistic does not create JSON indentation, comma or object-block churn. Paths use JSON Pointer: `~0` represents `~`, and `~1` represents `/`; array indexes start at zero and preserve their original order.

```tsv
path	type	value
	object	-
/samples	array	-
/samples/0	object	-
/samples/0/elapsed_ms	float	12.125
/samples/0/passed	boolean	true
```

Types distinguish `object`, `array`, `string`, `integer`, `float`, `boolean`, and `null`. Containers use `-`, and empty strings use `""`; empty containers, null and absent fields remain distinct. Tabs, newlines, backslashes and control characters in strings are escaped; trailing spaces use `\u0020` to avoid multiline records and Git trailing-whitespace warnings. Numeric values and integer/float types are retained without precision truncation.

The [conversion index](../../assets/benchmarks/conversion.tsv) records original JSON names/byte SHA256, TSV names/byte SHA256, canonical value digests, sizes and row counts. JSON names and hashes inside historical reports retain their original meaning; resolve the corresponding TSV through the index rather than treating an old hash as a TSV hash. Samples, failures, protocols, provenance and existing CSV/figures remain unchanged.

```bash
python3 benchmark/pvisor/evidence_tsv.py check docs/src/assets/benchmarks
python3 benchmark/pvisor/evidence_tsv.py convert \
  docs/src/assets/benchmarks/cluster-scalability-20261005/vm.tsv \
  /tmp/pvisor-vm-evidence.json
```

The converter reads and writes both formats. Reconstructed JSON retains values but does not promise the original whitespace or key order. Original bytes from this migration are also retained in `target/benchmark-json-originals-20261005/`, with every original SHA256 verified. This is a local recovery backup outside published attachments.

Current plotting and summary scripts accept TSV and can still read runtime JSON from new experiments. Reference-environment and Ubuntu publishers automatically convert public attachments to TSV. Runtime protocols and experiment output under `target/` retain their formats. Format checks and plotting neither start VMs nor rerun benchmarks.
