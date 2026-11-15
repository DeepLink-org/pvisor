# Local validation of feature tests and Criterion workflows

## Question and scope

Do the feature-scoped test system and Rust benchmark workflows execute correctly,
and does changing the measurement system change observed performance levels?

This is an engineering workflow validation, not a new user benchmark publication.
It exercises B-PROCESS, the native B-STARTUP reference, native B-FS-TOOLS probes
and B-REPLAY. It does not update published tables or combine incompatible cohorts.

## Artifacts and method

The product was built with:

```sh
mise exec -- cargo build --release --locked -p pvisor-cli \
  --features gateway --bin pvisor --bin pvisor-replay
```

| Artifact | SHA-256 |
| --- | --- |
| release `pvisor` | `d98942b1b8ba2c52f9e65c4a89f4630bcc9ab1745b5f3a85f99947fa1d6a9809` |
| release `pvisor-replay` | `a4b6f4a6453019ffd0c7b138b0060f4d1ac01fc2e639538cf1d71577d2533fc6` |

The process bridge uses exactly the same release binary for both engines.
The Python harness was extracted unchanged from Git revision
`0645a530a2f009e3ab59a1b52b26ef305e7f1bdb` into the ignored evidence directory.
Its timer source has Git blob `8c1118f47c7828c24be72a96e74aaa847f2697be`.
This is a one-time diagnostic, not an alternative supported measurement entry.
Both Python cohorts passed their frozen independent evidence validator.

Runs were serial, pinned to CPUs 0,1, in Python A → Criterion A → Python B →
Criterion B order, followed by native startup, filesystem and replay.
There were no concurrent Cargo/rustc/nextest processes in the recorded boundary
snapshots. The host was Fedora Linux 7.3.3-300.fc45.x86_64 on Ryzen 7 9700X,
with the existing powersave governor, boost and swap state left unchanged.
Load averages were approximately 3.5–4.5; CPU PSI avg10 at the recorded suite
boundaries was 0.00–0.95%. These are not frequency-locked or interference-free
laboratory results. Valid slow observations and outliers were retained.

Python smoke uses two warmups and ten individual samples per condition.
Criterion smoke uses ten Flat statistical samples plus adaptive calibration and
warmup. Criterion values below are `median.point_estimate` from `estimates.json`,
not the console's mean estimate or operation-level tail latency.

## Process measurement bridge

All values are milliseconds per operation.

| Condition | Python A median | Criterion A median | Python B median | Criterion B median |
| --- | ---: | ---: | ---: | ---: |
| `/usr/bin/true` | 0.192269 | 0.446042 | 0.387529 | 0.447377 |
| host Session run | 46.551489 | 46.899321 | 45.101873 | 46.176958 |
| status | 3.810224 | 4.142241 | 3.277695 | 4.136909 |
| review | 3.648156 | 4.031738 | 4.278908 | 4.360998 |

The host-run levels agree approximately, while the very short native command
has a larger observer/sampling contribution. Python's native median also varies
substantially between its two cohorts. Neither a constant correction nor pooled
samples are justified by these observations.

The Python timer includes its process-control and text-decoding path. The Rust
observer includes spawn, exit observation and pipe EOF, with concurrent readers
and owned-group supervision. Thus this compares measurement systems on one
product artifact, not a faster/slower candidate implementation.

The Criterion-only repeatability gate used the first saved baseline and second
run with the same product artifact:

| Condition | Median change | 15% gate |
| --- | ---: | --- |
| native | +0.30% | PASS |
| host-run | -1.54% | PASS |
| status | -0.13% | PASS |
| review | +8.17% | PASS |

This sequential gate is not a randomized treatment-effect experiment or evidence
of product improvement. A comparison between different product revisions still
requires separately frozen release artifacts under the same harness and inputs.

## Additional real Criterion workflows

Values are Criterion medians and their 95% median-estimate confidence intervals
in milliseconds per operation.

| Workflow | Median | 95% interval | Statistical samples |
| --- | ---: | --- | ---: |
| native startup Ready | 0.534306 | 0.510946–0.547894 | 60 |
| native startup normal exit | 1.646105 | 1.629746–1.658815 | 60 |
| native metadata worker | 2.241001 | 2.214034–2.288638 | 30 |
| native 64 MiB read/check worker | 40.608319 | 40.184768–41.650354 | 30 |
| replay claude-code prepare-only | 7.010245 | 6.629128–7.739802 | 60 |
| replay codex prepare-only | 7.680267 | 7.341562–8.148387 | 60 |
| replay opencode prepare-only | 6.814783 | 6.562201–7.001710 | 60 |
| replay mini-swe-agent prepare-only | 6.628783 | 6.553882–6.843589 | 60 |
| replay openhands prepare-only | 6.812215 | 6.655207–7.026967 | 60 |
| replay pi-agent prepare-only | 15.502217 | 14.338767–18.414254 | 60 |
| replay swe-agent prepare-only | 7.806696 | 6.552325–13.162232 | 60 |

Filesystem fixture preparation and validation are outside returned durations;
the worker's process and checked workload are inside them. Replay executes all
seven real CLI adapters with no model/tool calls and verifies prefixes, results
and unchanged workspaces. These rows are not the synthetic fixed-duration
measurement-boundary self-test.

The earlier published startup/replay/filesystem exports have different clocks,
sampling schedules, product artifacts and host environments. Their original
raw inputs/binaries are not available in this checkout. Their numbers remain
unchanged and are not treated as baselines for this table.

## Evidence and limitations

The integrated checkout also passed:

- `mise run test -- pvisor-cli -- --features gateway`: 368 passed, 85 skipped.
- `mise run test-benchmark-tools`: 167 Rust tests passed, one skipped;
  Python suites ran 364 tests with two skips.
- `mise run test-py`: 211 tests with 16 platform/environment skips.
- Release-binary strict feature cases: Docker 2, Compose 2, config 4, isolation 3.
- `mise run test-scripts`: includes 15 scenario resource/preservation tests and
  four feature-capability/discovery tests.
- Native Docker/Compose task partitions: 36 and 9 default tests passed.
- `mise run docs-build -- --require-recorded`: 135 bilingual pairs and 272 HTML
  pages passed after independent review of the 27 changed translation pairs.

Ignored and skipped tests are unexecuted coverage, not hardware acceptance.
Formatting, Python/workflow lint, the standalone Criterion Rust formatter and
all 80 task declarations were checked separately.

Local raw records are retained under:

```text
benchmark/process/.data/pr-engine-bridge-20261115T164557Z/
  frozen/                    exact Git harness sources and identities
  python-a/                  failed preparation: missing frozen compare helper
  python-a-valid/            validated Python cohort A
  python-b-valid/            validated Python cohort B
  criterion-a/, criterion-b/  per-operation evidence
  criterion-output/          Criterion samples, estimates and saved baseline
  repeatability-gate/        native Rust comparison report and JSON
  native-startup*/           native Ready/exit evidence and estimates
  native-filesystem*/        worker evidence and estimates
  native-replay*/            seven-adapter evidence and estimates
  *-environment.txt          host pressure, affinity and artifact identities
```

The initial preparation failure was resolved by extracting the missing helper
unchanged from the same Git revision. That failed directory was not reused,
counted as a sample or deleted. Source hashes, build command and product-source
snapshot are retained alongside the measurements.

Full KVM/HVF/WHP, OCI, DAX, nine-runtime filesystem, remote image supply and
memory campaigns were not run. Prepared firmware/rootfs/image and resource
receipts are required for those experiments. No new data was published, no
history was relabeled, and no specification approval was performed.
