# Benchmark methodology and environment

All benchmarks follow the same protocol:

- State the hardware, operating system, kernel or macOS version, FUSE implementation, pVisor version, and commit.
- Report p50, p95, and p99 with the sample count.
- Provide a script that reproduces the result in one command (under `benchmark/`), reporting through the `pvisor-benchmark/v1` schema.
- State the configuration of every control group; do not compare against untuned competitors.
- Keep results by date instead of overwriting old data.

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

Complete filesystem, network, supervision-cost, and concurrency-density measurements are still missing. Existing tools, passing specifications, or phase-specific figures cannot replace those results.

`benchmark/pvisor/vm_ready.py` uses `pvisor-vm-readiness/v1`, measuring workload readiness and CLI completion separately, with independent host-checkpoint diagnostics. See [startup latency](startup.md) for the protocol.
