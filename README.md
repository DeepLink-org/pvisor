# <img src="https://raw.githubusercontent.com/DeepLink-org/pvisor/main/docs/overrides/assets/logos/pvisor-icon.png" alt="PolicyVisor" width="72" /> PolicyVisor (pVisor)

Run a coding agent in a staged workspace, review its changes, and apply only the paths you keep.

运行 Agent，改动先进入暂存区，审查后再决定哪些写入工作区。

[![CI](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml)
[![Documentation](https://img.shields.io/badge/docs-latest-blue)](https://deeplink-org.github.io/pvisor/)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/DeepLink-org/pvisor/blob/main/LICENSE)

![Run an agent, review its file changes, apply the paths you keep](https://raw.githubusercontent.com/DeepLink-org/pvisor/main/docs/overrides/assets/logos/pvisor-social.png)

pVisor runs your existing Agent CLI, a script or an automation command. With
staging enabled the agent edits a copy-on-write view, so your working tree stays
unchanged until you apply a change. `apply` takes the paths you name and checks
the recorded preimage first, so it refuses a file that changed underneath it.
Every Job also writes a Run Bundle with the command, the effective policy, the
file changes, and which controls actually installed.

The basic workflow needs no Agent SDK and no model account.

## Run one Job

```bash
pip install pvisor-sandbox
cd your-project

pvisor spawn --safe -- claude      # your Agent CLI, or any script
pvisor review last                 # read the staged changes
pvisor apply last --path src       # keep src/
pvisor drop last                   # discard whatever you left staged
```

Replace `claude` with your Agent CLI or script. `--safe` requests the safety preset
and refuses an executor that cannot provide isolation and staging. `last` resolves
the latest Job registered for the current workspace; add `--diff` to print a
unified diff beside the classified change list. With `--stage PATH` the Job
registers against that path, so review and apply take the path instead of `last`.
Use the printed Job ID when managing several Jobs.

`--safe` stages writes, which needs FUSE: macFUSE 5.4.0 or newer with its FSKit
extension on Apple Silicon macOS, `/dev/fuse` and user namespaces on Linux x86_64.
If that is not set up yet, run `pvisor spawn -- claude`: the Job is still
recorded, and writes go straight to your workspace. Wheels are published for
Linux x86_64 and Apple Silicon macOS; VMs use KVM on Linux and
Hypervisor.framework on macOS and need a Linux image or rootfs. See the
[installation guide](https://deeplink-org.github.io/pvisor/start/installation/)
for prerequisites.

Other install methods (no Rust toolchain needed):

```bash
# Rolling nightly build
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/pvisor/main/scripts/install-nightly.sh | bash

# Homebrew (project tap)
brew tap DeepLink-org/formula https://github.com/DeepLink-org/Formula.git
brew install DeepLink-org/formula/pvisor
```

Stable and nightly GitHub Releases include native archives and SHA-256 checksums.
The stable workflow generates a Homebrew formula and can update the tap after its
[one-time setup](https://deeplink-org.github.io/pvisor/community/releasing/#one-time-setup).

Job startup requires the executable and its ancestor directories to be owned by
you or root and not writable by others. If Homebrew's Cellar is group-writable,
the command stops with `unsafe owner executable ancestor` and names the
directory; the [installation guide](https://deeplink-org.github.io/pvisor/start/installation/)
covers that case, platform requirements and executor setup.

For a minimal VM Job with a Linux image:

```bash
pvisor spawn --executor vm --rootfs image=ubuntu:24.04 -- /bin/echo hello
```

The [examples](https://github.com/DeepLink-org/pvisor/blob/main/examples/pvisor/README.md)
cover filesystem isolation, selective apply, network policy, model request capture
and interactive review.

## Measured results

| Workload | pVisor | Same experiment |
| --- | --- | --- |
| Environment ready, warm caches (VMs at 2 vCPU / 128 MiB) | VM 130.71 ms; host 33.52 ms; staged 50.59 ms | rootless Docker 105.03 ms; Firecracker 287.13 ms; QEMU microvm 260.10 ms |
| 10,000 files, 20 changed, 10 kept | 141 ms staged | Git worktree 248 ms; btrfs reflink 343 ms |
| Completed tasks, 2 cores / 2 GiB | staged host 160/160 at 32 concurrent; VM 80/80 at 16 | Podman 160/160 at 32 |

The rows come from separate experiments with their own pinned configurations.
Values move with platform, workload and image; the pages linked from
[benchmarks](https://deeplink-org.github.io/pvisor/benchmarks/) state the conditions
and the full tables, including results that do not favor pVisor.

## Other capabilities

| Capability | What it does |
| --- | --- |
| Policy admission | Resolves the executor and requested capabilities before dispatch, and records the available controls and observations. |
| Network and model control | OverlayNet applies network policy; optional Gateway support adds model protocol handling and request/response capture. |
| Checkpoint and fork | Supports logical filesystem checkpoints and forks; execution snapshots are separate, restricted VM capabilities. |

Execution is bounded, reversible and checkable within the documented scopes.
Inspect the Run Bundle's evidence and warnings to see which controls were
installed; requesting a policy does not prove enforcement. Logical checkpoints
do not capture process memory, and replay does not roll back an external service.

## Execution boundaries

A Job can run as a host process, through the native OCI container executor, or
in a Linux VM. Each executor has different isolation boundaries, controls and
prerequisites.

| Executor | Boundary and current limits |
| --- | --- |
| Host | Uses platform controls and optional filesystem staging. Selective network proxying is cooperative unless a mandatory network boundary is configured. |
| OCI container | Delegates execution through an OCI runtime. It does not satisfy the `--safe` preset; delegated result provenance remains a limitation of this implementation. |
| VM | Runs a separate Linux guest through `pvisor-vm`, with virtio-fs and a userspace network data plane. Supported native VMM targets are Linux x86_64/KVM and macOS ARM64/HVF. |

pVisor trusts the host kernel, its supervisors, and the host account's management
code and configuration. The project does not claim a hardened boundary between
mutually hostile tenants sharing that account.

The following limits apply across executors:

- Remote API calls, database writes and sent messages are outside `apply`/`drop`
  and remain in effect. Dropping a stage also leaves files that have already been
  applied in place.
- Apply does not provide a multi-file transaction against other writers. Stop
  concurrent editors and other apply processes while accepting a batch. Conflict
  detection and recovery records do not make arbitrary concurrent writes safe.
- Proxy records cover traffic through the proxy. Missing observations do not
  prove that an action never happened.
- Crash recovery is incomplete. Attempt completion publishes several durable
  records separately, so a host-process crash between them can require reconciliation.

See [capabilities and evidence](https://deeplink-org.github.io/pvisor/concepts/capabilities-and-evidence/),
[staging semantics](https://deeplink-org.github.io/pvisor/concepts/staging/) and the
[security documentation](https://deeplink-org.github.io/pvisor/security/) before
relying on a boundary.

## Other ways to use pVisor

The repository also provides these entry points, each with its own contract:

| Entry point | Scope |
| --- | --- |
| `pvisor spawn` | Native Jobs: run, review, apply/drop, inspect, checkpoint and fork. |
| `pvisor run`, `build`, `pull`, `exec` | Experimental Docker-style CLI subset, with one Linux VM per container and no central Docker daemon. This is separate from the OCI executor used by Jobs. |
| `pvisor-daemon` | Single-node services for sandbox APIs, cache and optional memory pools. Sandbox serving implements partial OpenSandbox 1.1.0 and E2B Python SDK 2.59.0 profiles on Linux/KVM. |
| `containerd-shim-pvisor-v2` | Linux containerd integration with its own lifecycle and platform requirements. |
| Rust library `pvisor` | Embeddable execution runtime and durable Job service. |
| `pvisor-tui`, `pvisor-replay` | Interactive Job frontend and replay/continuation tools. |

The container CLI does not implement the Docker Engine API. The daemon's sandbox
API does not expose native Job stage/apply or checkpoint/fork semantics. SDK
support is limited to the partial profiles described above. External
orchestrators handle cross-node placement, workflow scheduling and retries.

Use the [container command profile](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-cli/README.md#docker-command-profile-experimental),
[daemon contract](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-daemon/README.md)
and [shim guide](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-shim/README.md)
for supported operations and limitations.

## VM density and current maturity

pVisor includes a shared compressed image pool, optional RAM sharing, VM
pause/offload controls and experimental guest-idle/model-wait integration.
Support depends on the platform, RAM backing and execution profile. Some features
require opt-in, and availability varies across frontends.

The runtime feature `workload-aware-memory-offloading` currently enables only
vCPU wait observation. It does not automatically reclaim memory. Shared memory
pools and Apple Silicon shared kernel baselines are separate opt-in mechanisms.
See the [runtime experiments](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor/README.md#explicit-runtime-experiments)
and [VM implementation](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-vm/README.md)
for their contracts.

The project is a developer preview. Architecture reviews and targeted tests cover
individual behaviors; they do not establish stable crash recovery, isolation
between hostile tenants or full OCI/SDK compatibility. Assess platform integration,
release-artifact validation and long-running fault tests for the revision and
configuration you use. Memory sharing alone does not establish sandbox density
or net memory savings. Consult the
[benchmarks](https://deeplink-org.github.io/pvisor/benchmarks/) for measured
workloads and conditions.

## Architecture and development

The workspace has 13 crates, grouped by the responsibilities they own:

![Agent CLI, admission, executor and Run Bundle in one Job](https://raw.githubusercontent.com/DeepLink-org/pvisor/main/docs/overrides/assets/diagrams/pvisor/system-products.svg)

| Layer | Crates and responsibility |
| --- | --- |
| Applications | `pvisor-cli` owns `pvisor`, `pvisor-tui` and `pvisor-replay`; `pvisor-daemon` and `pvisor-shim` own service integrations. |
| Runtime | `pvisor` owns admission, Jobs, Sessions/Attempts, executors and driver orchestration. |
| Shared contracts and persistence | `pvisor-core` defines policies, Operations, Events and shared records; `pvisor-journal` owns durable event storage. |
| Filesystem | `pvisor-overlay-core` owns shared overlay/apply mechanics; `pvisor-overlayfs` supplies the host filesystem adapter; `pvisor-vm` supplies virtio-fs. |
| Network and models | `pvisor-overlaynet` owns network policy transport and the VM data plane; `pvisor-gateway` owns model protocols and capture. |
| Virtual machine and guest | `pvisor-vm` owns the VMM and RAM lifecycle; `pvisor-guest` owns guest process execution and reaping. |
| Replay | `pvisor-replay` implements replay using Core and Journal contracts, independently of the runtime and CLI parser. |

For local development:

```bash
mise run doctor -- dev
mise run build -- release           # Supported executables for this host
mise run test -- pvisor pvisor-cli   # Runtime and frontend tests through nextest
mise run test                    # Workspace Rust tests, then Python suites
```

Run checks for the affected packages when making focused changes. `mise run build -- PROFILE COMPONENT`
supports `debug`, `release` and `performance` profiles and `all`, `product`,
`daemon`, `shim` and `shim-vm` components. Builds prepare the customized guest
firmware from [`fw/`](https://github.com/DeepLink-org/pvisor/blob/main/fw/README.md);
published wheels do not require a local kernel
build toolchain. See the [engineering guide](https://deeplink-org.github.io/pvisor/community/development/)
for dependencies, platform setup and validation.

## Development environment

Install [mise](https://mise.jdx.dev/installing-mise.html) 2026.8.5 or newer, then run
`mise trust`, `mise install --locked`, and `mise run setup`. The checked-in `mise.toml`
and `mise.lock` pin a reproducible default toolchain. CI and wheel builds use mise
2026.10.18. You can select compatible local tools in the gitignored
`mise.local.toml`; see [local tool overrides](https://github.com/DeepLink-org/pvisor/blob/main/CONTRIBUTING.md#local-tool-overrides).
`uv.lock` fixes Python development dependencies in
`.venv`. Use `mise tasks ls` to list tasks and `mise run test` to validate.
The timed peer TCP recovery test reserves all nextest execution slots while it
runs; other tests keep their normal parallelism. See [.config/nextest.toml](https://github.com/DeepLink-org/pvisor/blob/main/.config/nextest.toml).
See [contributing](https://github.com/DeepLink-org/pvisor/blob/main/CONTRIBUTING.md)
for the development loop and platform prerequisites.

Task implementations that need more than a short command live in `scripts/tasks/`.
Their names and shared environment are declared in `mise.toml`. CI installs tool
subsets for Python, semspec, documentation and release checks. Only full setup
prepares guest targets. The optional Podman Go contracts use their pinned
`setup-go` environment; install native libraries and virtualization prerequisites
with the host package manager.

## Guest firmware

The customized libkrunfw sources, guest kernel configurations, patches and bundle
tests live in [`fw/`](https://github.com/DeepLink-org/pvisor/blob/main/fw/README.md). CLI, daemon and single-wheel builds use this
source-built firmware by default, with verified kernel downloads and build caches
keyed by their inputs. Use `mise run fw -- build` to build the firmware independently
and `mise run fw -- test` for bundle/ABI regression tests. The directory README covers
toolchain requirements, cache settings and corresponding-source export. Published
wheels install without a kernel build toolchain.

## Documentation and community

- [First run](https://deeplink-org.github.io/pvisor/start/first-run/): the run-review-apply workflow
- [Why pVisor](https://deeplink-org.github.io/pvisor/why/): goals, use cases and the trust ladder
- [CLI reference](https://deeplink-org.github.io/pvisor/reference/cli/): commands and configuration
- [Architecture](https://deeplink-org.github.io/pvisor/design/architecture/): component boundaries and execution flow
- [Experimental features](https://deeplink-org.github.io/pvisor/design/experimental/): stages and validation gates
- [中文文档](https://deeplink-org.github.io/pvisor/zh/)

Contributions: [CONTRIBUTING.md](https://github.com/DeepLink-org/pvisor/blob/main/CONTRIBUTING.md).
Questions and setups: [Discussions](https://github.com/DeepLink-org/pvisor/discussions).
Security reports: [SECURITY.md](https://github.com/DeepLink-org/pvisor/blob/main/SECURITY.md).

## License

[Apache License 2.0](https://github.com/DeepLink-org/pvisor/blob/main/LICENSE). See
[NOTICE](https://github.com/DeepLink-org/pvisor/blob/main/NOTICE) for third-party
attributions and separately licensed bundled components.
