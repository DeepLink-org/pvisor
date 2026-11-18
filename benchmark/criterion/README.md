# Criterion experiments

This Rust tool is the measurement entry for the registered pVisor experiments.
Criterion **0.7.0** owns calibration, sampling, estimates and baseline comparisons.
The modules own fixture preparation, operation boundaries, correctness and cleanup.
No Python measurement process is used.

The default `storage-tls` feature supplies host-only private HTTPS registry
support. Build static guest fixtures with `--no-default-features` so their
compilation does not require a cross-built OpenSSL. This does not disable any
default host experiment; optional product-linked probes use `vm-probes`.
The prepared startup/image, filesystem, network and storage adapters require
Linux. The independent macOS allocator adapter remains in the memory family;
Unix process/replay capability is separate. Unsupported families return an
explicit error rather than preventing the host tool from compiling.

`cargo run --release --locked --manifest-path benchmark/criterion/Cargo.toml --
smoke --binary /absolute/path/to/pvisor` runs the process suite. `--smoke` runs
the small, tool-independent harness regression benchmark instead; it is not a
product performance result. Pass Criterion baseline options after `--`, for
example `-- --save-baseline candidate`. `--list` lists registry entries without executing.
An entry marked unprepared still fails before any experiment starts.

Each `iter_custom` returns the sum of the durations of exactly the operations
requested by Criterion. Fixture generation, parsing, validation and evidence
writes are outside that duration. Each invocation, including warmup and
calibration, must validate; failure stops the run, not a zero or skipped sample.
Operation observations are separate from Criterion's batch estimates. Do not
calculate operation P95/P99 from Criterion batch averages.

The Criterion cohort is a **new statistical protocol**: a sample contains one
or more operations, and calibration/warmup also invoke operations. Legacy
fixed command counts are not Criterion sample counts. Flat sampling and the
registered minimum sample sizes avoid quadratic iteration growth, but do not
promise an exact operation count. Never merge these results with historical
Python/hyperfine cohorts or publish using a legacy binding.

Evidence is retained in a fresh `--output` directory, including failed runs;
without that option the tool chooses a new `.data/` directory. Product artifacts
must already be built and provisioned. The tool never publishes documentation.
Existing Python scripts that read historical evidence or draw figures are a
display/archive layer, not this measurement path.

For lazy VM, stage and remote storage experiments, build the cache client probe
with the product toolchain and profile (`cargo build --locked --release -p pvisor
--example cache_probe`). Leave it under `examples/` beside the product binaries.
The harness freezes it for setup outside the measured interval.
