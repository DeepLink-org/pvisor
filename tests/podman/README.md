# Podman-derived Docker CLI contracts

This directory retains complete copies of Podman's `test/system/` and
`test/e2e/` under `upstream/test/`, plus an adapted supported-command profile
in `system/` (Bats) and `e2e/` (Go). See `UPSTREAM.md` for the exact revision,
license and adaptation map. The upstream copies are reference material: they
are not executable pVisor gates and must not be included in normal discovery.

The adapted suites drive the top-level pVisor container commands as a black box and check expected
outputs, exit codes and lifecycle effects, following Podman's system/e2e model.
They require no Docker executable, daemon, socket or comparison run. Cases that
specifically interoperate with a real Docker daemon are excluded with reasons.
The suites use neither Podman storage nor pVisor private APIs. Go uses its
standard-library test runner instead of importing Podman's Ginkgo harness;
Bats retains its upstream framework.

## Run

Build pVisor first with `just build`. Install Bats 1.5+, Python 3.10+ and Go 1.20+ separately if needed.
From the repository root:

```sh
PVISOR_BINARY="$PWD/target/debug/pvisor" bats tests/podman/system
PVISOR_BINARY="$PWD/target/debug/pvisor" go test -v tests/podman/e2e/*.go
```

On Apple Silicon, the executable needs the Hypervisor entitlement and its
adjacent `libkrunfw.5.dylib`. To sign a private copy without changing your build:

```sh
runtime_dir=$(mktemp -d)
cp target/debug/pvisor "$runtime_dir/pvisor"
ln -s "$PWD/target/debug/libkrunfw.5.dylib" "$runtime_dir/libkrunfw.5.dylib"
codesign --force --sign - --entitlements crates/pvisor/macos-hypervisor.entitlements "$runtime_dir/pvisor"
export PVISOR_BINARY="$runtime_dir/pvisor"
# Run either suite using PVISOR_BINARY, then remove runtime_dir.
```

These are special external-runtime runners, separate from `just test`.
`PVISOR_BINARY` selects a single executable path (default: `pvisor` in PATH),
not a shell command string. Arguments are passed directly to the top-level CLI; there is no engine selector. Missing pVisor, image pull failures or unavailable
virtualization fail active cases rather than skip them.

Each test owns a private `PVISOR_DOCKER_HOME`, a unique container name and bounded
command timeouts. Cleanup removes that container and the private store, including
its test images and named volumes. `PVISOR_TEST_IMAGE` defaults to
`docker.io/library/alpine:3.20`; set a pinned Alpine manifest digest for repeatable
runs and record `pvisor --version` and host architecture. Bats uses Python 3 for
bounded process execution; Go uses `context` deadlines.

## CI

The required `rust-test` Linux `cli` shard in `.github/workflows/ci.yml` runs all
active adapted Bats and Go contracts on every covered push/pull request. It
reuses the product build and KVM permissions from that shard, installs Bats and
Go 1.23.12, and runs the CLI as the ordinary runner user. The fixture is
`mirror.gcr.io/library/alpine:3.20`; pull logs record the resolved digest.
Unavailable KVM, registry failures and contract failures fail the gate, with no
fallback to a skipped test. Hosted Apple Silicon macOS runners lack nested
virtualization, so this runtime gate uses Linux/KVM.
The selected executable is the original static musl product artifact under
`target/x86_64-unknown-linux-musl/debug/`, whose firmware is embedded. Native
Cargo tests can overwrite `target/debug/pvisor` with a dynamically linked test
build, so that path must not be used for this external-runtime gate.

Both suites are attempted even when one fails. The combined step is bounded by
30 minutes; Go has a 20-minute suite timeout and `-count=1` disables cached test
results. Per-command timeouts and test-owned cleanup remain in effect. The
`podman-contracts-linux-amd64` artifact contains Bats TAP, Go JSON output and
environment/version metadata, and is uploaded even on failure. The existing
required `CI` aggregate includes this shard. A newly added gate still needs an
actual hosted-runner pass; local macOS acceptance does not establish Linux/KVM
acceptance.

## Coverage and limits

The active profile has 35 Bats cases and 83 Go cases. Each test first pulls its
image; none is skipped based on engine failure.

| Suite | Active contracts |
| --- | --- |
| Bats `030-run.bats` | Workload exit, missing executable, stdin/EOF, env/workdir, bind read/write, read-only creation rejection |
| Bats `031-run-options.bats` | Explicit env vs env-file, successive env-files, host env import, literal entrypoint args, numeric/named user, missing workdir creation, workdir within bind mount, read-only deletion rejection |
| Bats `032-run-boundaries.bats` | Exit 126 for directories/non-executable files, failed-run name cleanup, host environment isolation, empty values, option-like workload arguments, relative-workdir rejection, no non-loopback interfaces with `--network none` |
| Bats `033-run-validation.bats` | Repeated/literal environment values, missing host variable removal, CRLF env-files, missing bind source rejection, conflicting mount destinations and paths containing spaces |
| Bats `070-build.bats` | Cached-base build/run/removal, relative external Dockerfile and TMPDIR, ignore-all definition discovery, external definition with ignore-all, symlink context and rejected ignored COPY source |
| Go `build_test.go` | Basic build/removal, iidfile/full-ID execution, missing definition/file context errors, symlink workdir, copied empty directories/symlinks, ignore filtering, cross-directory COPY hardlinks, default COPY ownership, build arguments and WORKDIR/CMD/ENV/LABEL defaults |
| Go `cli_test.go` | Streams/exit, exec stdin/error, stop/start persistence, auto-removal, duplicate name, exec env/workdir isolation, exec user/streams, 1.7 MB stdin round trip, ps/ID lifecycle, retained logs, images/rmi, named-volume persistence across rm -v, volume image initialization/read-only reuse, running-container rm rejection/force |
| Go `compatibility_test.go` | Closed stdin, 1 MiB stdout, run/exec TTY and TERM precedence, concurrent exec exit routing after failed spawns, repeated stop, custom stop signal and zero-timeout escalation, multiple-container isolation, partial batch failures, live/stopped `logs -f`, ephemeral IPv4 TCP publication with half-close, in-use image protection, short image IDs and repeated pull identity |
| Go `lifecycle_test.go` | Anonymous volume restart/isolation, live named-volume sharing including read-only readers and namespace changes, copy-up preservation, volume aliases/order, bind restart and symlinks, read-only mutation families, exact binary/partial logs, log accumulation across starts, running-start idempotence, automatic/forced removal races, TCP port restart/release/conflict and multiple endpoints |
| Go `process_test.go` | Large exec output, binary stdin, env unsets/literal argv, non-executable/directory/missing-workdir errors, live passwd lookup, inherited run identity, empty TERM, in-flight exec termination, workload exit 125 and signals, umask isolation, full/short image-ID execution, unknown filter, batch/forced rmi, named/numeric primary groups, symlinked workdir, default CMD/empty entrypoint and invalid names |

The continuing source audit, outstanding supported scenarios and proposed
cost-based deferrals are in [REVIEW.md](REVIEW.md). The audit remains open;
these counts do not mean the entire imported corpus has been covered.

An independent, opt-in macOS [lightweight chaos runner](CHAOS.md) mixes these
operations with client disconnects and concurrent starts under fixed resource
limits. Chaos rounds are not counted as additional Podman contracts. The normal
Go suite includes its concurrent-start regression, which verifies one entrypoint
execution for each pair of simultaneous starts.

Together these exercise all eleven registered container/build subcommands: `pull`, `images`, `rmi`,
`run`, `build`, `exec`, `ps`, `start`, `stop`, `rm`, `logs`. They do not cover every option.
The volume-image-content fixture requires an Alpine image containing
`/etc/alpine-release`, `nobody`, `sh`, `printenv`, `id`, `cat`, `sleep` and `true`.
Use `PVISOR_TEST_IMAGE` for a pinned Alpine digest, not an arbitrary image.
The default-command cases expect Alpine's `/bin/sh` CMD and empty entrypoint;
identity/workdir cases also use its `bin`, `nobody`, `nogroup` entries and
`/var/run -> /run` symlink. `nogroup` is GID 65533, while `nobody` is 65534.

Explicitly unsupported commands/options are preserved as commented examples in
`system/900-unsupported.bats.disabled` and `e2e/unsupported_test.go.disabled`,
with source files and reasons. Their `.disabled` suffix excludes them from test
discovery; they produce no PASS or SKIP results. Complete upstream sources stay
unchanged. Buildx and unsupported build options, Compose/API, create/inspect/wait/restart/attach/cp, registry login,
Podman pods/Kube/Quadlet, management CLIs, extra logs/list flags and host-specific
namespace/security controls are disabled. Anonymous-volume deletion, controlling-terminal
input, publication on non-loopback host addresses and other supported-but-unported scenarios
remain coverage gaps, not unsupported features.

Build ports use the documented Docker subset. Podman-specific `--format=docker`,
`--pull-never`, COMMIT log rendering and Containerfile auto-discovery are not
claimed. Public `images`/`run` and OCI tar output replace unsupported inspect for
image identity, filesystem and label observations. Build validation errors use
Docker-profile exit 1 rather than Podman 125. Remote ADD cases retain only their
local COPY filtering/link contracts, not remote protocol or ADD coverage.
The original RUN-exit-code relay case (Podman exit 5) remains unported: pVisor
currently reports a failed build as exit 1. Adaptations and remaining build gaps
are recorded in [UPSTREAM.md](UPSTREAM.md) and [REVIEW.md](REVIEW.md).

The TTY cases require default `TERM=xterm`, explicit TERM precedence and no
mutation of container defaults by `exec -t`. These defaults are applied on the
host; the generic guest launcher is unchanged. TCP uses the pinned Alpine's
BusyBox netcat, explicitly bound to guest IPv4 loopback, and verifies the full
echo after the client closes its write half. No additional image package is
installed. Go invocations must include all active `*_test.go` files as above.

Shared-volume regressions require reopened files to see completed writes from
another VM or mount, including changes in file length and directory entries.
Ordinary passthrough mounts now use zero metadata validity with existing
close-to-open data caching. This does not establish coherence for already-open
mappings or unsynchronized concurrent writes. Missing bind sources are rejected
under pVisor's documented stricter profile; Docker's automatic host-directory
creation is not claimed. Anonymous-volume storage deletion remains unobserved
without a public volume listing API; the active cases prove restart persistence
and isolation, not garbage collection.

A pass means the selected behavior contract held for pVisor. Dynamic IDs and
human-readable list formatting are not compared with another engine. Unsupported
features and Docker-daemon interoperability cases are explicitly excluded, never
counted as passes. Do not weaken assertions or skip a supported operation because
pVisor fails it. Other upstream cases without a port remain unvalidated.

For example, upstream `test/e2e/push_test.go` includes "podman push to docker
daemon", which invokes both Podman and the real Docker CLI. That case is recorded
as disabled in `e2e/unsupported_test.go.disabled`; no installed Docker is needed
to discover or execute the active profile.

## Non-root identity regression

The numeric/named `run -u` cases and `TestExecUserAndStreams` guard a regression
where the synthetic DNS lower exposed private host directory permissions as guest
`/` and `/etc`. Those directories became `0700`, so non-root commands exited 126.

Resolver setup now retains the image directories' guest ownership and modes in
virtual metadata while leaving the host resolver directories `0700`. If the image
has no `/etc`, the injected guest directory is root-owned `0755`; the resolver
file is root-owned `0644`. Explicit restrictive image permissions are preserved.
The original success assertions remain active. To run just these regressions,
select the newly built, Hypervisor-signed binary using `PVISOR_BINARY`:

```sh
bats --filter 'numeric user|named user' tests/podman/system
go test -v -run TestExecUserAndStreams tests/podman/e2e/cli_test.go
```

On 2026-10-21, all 29 cases passed on macOS ARM64 with the repaired executable,
alongside seven selected Rust Docker-module regressions and four Python harness
checks. Three unrelated cases initially hit Docker Hub HTTP 429 during setup;
they passed on retry via `mirror.gcr.io/library/alpine` using the verified same
ARM64 manifest digest
`sha256:45e09956dc667c5eff3583c9d94830261fb1ca0be10a0a7db36266edf5de9e1d`.

Linux/KVM execution remains unverified by the local macOS ARM64 VM run. Docker
comparison is not part of this suite or an acceptance prerequisite.

The expanded profile was validated on 2026-10-21 with Go 1.23.12 and Bats 1.11.0:
all 53 runtime cases passed (23 Bats, 30 Go), using the pinned ARM64 mirror
manifest above and a private copy of the rebuilt, Hypervisor-signed executable.
All 20 selected Rust Docker regressions and four Python adapter/source-integrity
checks passed; Clippy, rustdoc and formatting checks passed too. The new TTY
cases exposed missing default TERM in run/exec; the host-side fix preserves
explicit values and leaves guest supervision unchanged. No active cases were
skipped, no assertions were weakened, and upstream file hashes remain unchanged.

A second expansion on 2026-10-21 brought the profile to 76 cases (29 Bats,
47 Go), adding 23 cases to the 53-case profile. All 76 passed on macOS ARM64
with the same immutable Alpine mirror manifest, Go/Bats versions and a rebuilt,
Hypervisor-signed private binary. The new volume cases exposed stale metadata
across shared mounts/VMs; zero passthrough metadata validity fixes reopened-file
contents/lengths without changing data-cache policy. A complete run also exposed
force-stop publication racing automatic removal; catalog locking now protects
lookup/publication while termination waits remain outside the lock. The new
eight-iteration removal regression passed without retries or relaxed assertions.

All 31 selected Rust Docker/VM contract and boundary tests, both native HVF
container/Job acceptance cases and all four Python adapter/source-integrity
checks passed. Clippy, rustdoc with warnings denied, formatting and CI budgets
passed. Guest code and upstream sources were unchanged. Linux/KVM execution,
cross-VM open-mapping coherence and performance effects remain unvalidated.

The next batch was validated on 2026-10-22: all 101 runtime contracts passed
(29 Bats, 72 Go), including 25 added process/identity/image contracts. The same
signed executable and immutable ARM64 Alpine mirror manifest were used; this
batch changes tests and audit documentation only. Go vet, gofmt, diff checks
and all four Python adapter/source-integrity tests passed. Initial fixture
assumptions were corrected against the image: use BusyBox printf rather than
an absent `/bin/printf`, and assert Alpine's actual nogroup GID 65533. No
runtime failure was converted to a skip. The source audit in `REVIEW.md`
remains open, with further supported cases still to port and deferred areas
listed separately for human review.


The build batch was validated on 2026-10-23 on macOS ARM64/HVF with Go 1.23.12,
Bats 1.11.0 and the immutable Alpine mirror manifest recorded above. All 16 new
build contracts passed (6 Bats, 10 Go); this is not a rerun of all 118 active
contracts. The upstream cross-directory COPY hardlink case found that context
capture and COPY split hardlinks before export. Both paths now retain private
inode identity; all original selected inode-equality assertions pass. Nine Rust
builder regressions, four Python adapter/source-integrity checks, Go vet and
Clippy passed. Upstream copies and their hash inventory remain unchanged.
Linux/KVM and the remaining build cases are still unvalidated.

On 2026-10-24 the complete active profile passed on Linux x86_64/KVM with the
default compressed local image pool: 35 Bats and 83 Go contracts, no skips.
The immutable Alpine manifest was
`mirror.gcr.io/library/alpine@sha256:c64c687cbea9300178b30c95835354e34c4e4febc4badfe27102879de0483b5e`;
the runners were Bats 1.11.0 and Go 1.26.10. A frozen static musl product was
used throughout each run. Read-only observation of 115 private test stores
confirmed pooled catalog entries and no persistent legacy blob/rootfs/build-image
directories. This validates the active profile, not the entire upstream corpus.

The same candidate passed all four opt-in native Docker/Job acceptance cases
and a real-guest mmap probe spanning multiple 1 MiB pool blocks, including
read-after-close, private COW, shared-write/msync copy-up and original-image
preservation in a fresh container. The regression found and fixed local cache
diagnostics leaking into workload stderr and Linux rootless virtual CHOWN.
These are correctness results; they establish no mmap performance advantage.
Separate workspace benchmark-registration, CPUID doctest and opt-in Job
execution-fork failures, plus an intermittent filesystem/S3 cache-guest exit
125, remain outside this passing container profile.
