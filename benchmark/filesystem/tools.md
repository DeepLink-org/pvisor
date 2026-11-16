# Native filesystem: metadata and read

Benchmark: B-FS-TOOLS ([注册表](../REGISTRY.md#b-fs-tools)), role user-facing.

## Motivation

Measure complete native Rust metadata/read commands over fixed warm-cache inputs.
These probes do not represent VM, seven-tool phase or cross-runtime task costs,
and do not support paired improvement claims.

## Experiment design

Both rows share 32 directories, 2,048 small files and a 64 MiB data file.
The Rust worker streams fixture generation and verifies the exact inventory,
types and complete contents outside each measurement. The measured child checks
metadata counts/sizes or reads the complete data file and checks SHA-256.

Criterion 0.7 owns sampling, warmup and statistics (30 statistical samples).
Every invocation, including calibration and warmup, starts a new Rust worker,
has a 30-second process-group deadline, requires exact success JSON and rechecks
the complete fixture before and after. The clock spans process creation through
normal exit, stamped by the waiter rather than the periodic observer. Fixture
preparation and subsequent audit are excluded. This is a new Rust whole-command
metric: it does not include a Python interpreter and must not replace historical
Python/hyperfine observations or their provenance.

```sh
mise run benchmark -- filesystem/tools
# Select a Criterion benchmark filter using the parent runner's filter option.
mise run benchmark -- filesystem/tools --output benchmark/filesystem/.data/native-new
```

The entry is a locator, not a shell command. The parent Rust crate wires
`filesystem::register` and the `filesystem-worker` binary. Set
`PVISOR_BENCH_FS_WORKER` to an explicitly built worker when running the binary
directly, and `PVISOR_BENCH_OUTPUT` to a new directory under `.data`.

```json
{
  "engine": "criterion",
  "entry": "filesystem/tools.md",
  "implementation": "../criterion/src/filesystem/mod.rs"
}
```

## Data and analysis

Each actual invocation retains its command, stdout/stderr, completion time,
exit result and host observations. Criterion retains its own statistical samples.
The raw invocation inventory includes calibration, not just statistical samples;
do not infer a historical fixed 3/30 inventory from Criterion's sample count.
Failures and timeouts abort the benchmark and retain diagnostic evidence.

Historical Python batch auditing remains read-only in `audit.py` and
`historical.py`. No historical result, table or artifact is rewritten by the
Rust runner. See [README](README.md#rustcriterion-active-experiments) for the
layered and engineering protocols and their execution requirements.
