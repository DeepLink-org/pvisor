# Benchmark methodology and environment

All benchmarks follow the same protocol:

- State hardware, OS, kernel/macOS version, FUSE implementation, pVisor version and commit;
- Report p50, p95, p99 and sample count;
- Provide a one-command reproducer under `benchmark/`, using the `pvisor-benchmark/v1` report schema;
- State every comparison configuration; avoid comparisons with unconfigured alternatives;
- Keep results by date rather than overwriting historical data.

## Environment checklist

Start each report with this table:

| Item | Example |
| --- | --- |
| Date | 2026-10-02 |
| pVisor version and commit | `0.x.y` / `abc1234` |
| Hardware | Apple M4, 16 GiB; or CPU model, cores and memory |
| OS and kernel | macOS 26.x; or Ubuntu 24.04, Linux 6.8 |
| Filesystem and FUSE | APFS + macFUSE 5.x; or ext4 + libfuse 3.x |
| Executor and options | `--executor vm --overlaynet auto` |
| Samples and warmup | 100 runs, discard the first 5 |

## Controls

- Use each alternative's documented recommended configuration; state versions and options;
- Isolate pVisor's own cost: for example, compare `--filesystem host` with staging under the same executor rather than only reporting total time;
- When measuring only one phase, such as guest init, name that phase in the title rather than claiming end-to-end results.

## Start with existing measurement entry points

```bash
just benchmark
just benchmark nightly target/pvisor-benchmark/nightly
just benchmark-compare target/pvisor-benchmark/candidate/raw-report.json target/pvisor-benchmark/main/raw-report.json
# Linux：启动与资源占用矩阵
just benchmark-startup --warmups 10 --samples 100
```

`just benchmark` measures process-level costs of a minimal host Run and reading its Bundle: smoke uses 2 warmups/10 samples; nightly uses 10 warmups/50 samples. Both use `pvisor-benchmark/v1`. `benchmark-startup` writes separate `startup.json`/`startup.md` rather than claiming the same schema; its default is 3 warmups/30 samples, overridden above.

Existing startup trials exclude building, image downloads and rootfs preparation; state this boundary. Executors failing preflight are listed as SKIP with stderr retained. Successful trials require command success and completed/zero-exit Bundles. Failures, skips or degraded controls cannot count as faster successful samples.

## Interpretation and archives

Compare candidate/baseline on the same host, suite and inputs. `benchmark-compare` defaults to a 15% regression threshold; reports remain informational unless `--fail-on-regression` is enabled. Retain raw samples, summaries, complete options, input hashes and commits. Distinguish cold images, warm disk caches and warm page caches; record background load/power state.

Complete filesystem, network, supervision-cost and density measurements are still missing. Existing tools, passing specifications or phase-specific figures do not replace those results.
