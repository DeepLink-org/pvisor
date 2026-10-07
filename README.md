# <img src="docs/overrides/assets/logos/pvisor-icon.png" alt="PolicyVisor" width="72" /> PolicyVisor (pVisor)

**Scaling autonomous agent execution.**  
**让自主 Agent 的执行可以规模化。**

Today: run agents unattended and keep only the file changes you approve.  
今天：让 Agent 全自动执行，文件改动由你决定去留。

[![CI](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml/badge.svg)](https://github.com/DeepLink-org/pvisor/actions/workflows/ci.yml)
[![Documentation](https://img.shields.io/badge/docs-latest-blue)](https://deeplink-org.github.io/pvisor/)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Agent CLIs increasingly ship their own sandboxes, and those do block actions. What they do not give you is a post-hoc selective merge, a checkable record, or the same semantics across agents and executors. PolicyVisor adds those three properties: the workload runs unattended inside a policy boundary, and you review the result like a pull request—keeping only the changes you want.

The limit on agent autonomy is not compute; it is human supervision. Today every
unit of agent work costs roughly the same unit of human attention, so supervision
cost grows with the execution volume. PolicyVisor makes each execution **bounded,
recoverable, and checkable**, so supervision can be spread out, sampled, and
eventually automated.

![PolicyVisor execution and review workflow](docs/overrides/assets/diagrams/pvisor/system-products.svg)

## What you get

- **Hands off.** Let the agent finish unattended. Its file changes land in a
  stage; network and sensitive paths follow your policy.
- **Gate the result.** Review the changeset like a PR, apply the paths you want,
  discard the rest. If you changed the same file meanwhile, pVisor refuses to
  overwrite your edit.
- **Keep a record.** Every run leaves a checkable record of the limits actually in
  effect, the access that was blocked, and optional model requests.

## Install

```bash
pip install pvisor
pvisor --version
```

The rolling nightly build installs the same command without a Rust toolchain:

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/pvisor/main/scripts/install-nightly.sh | bash
```

See the [installation guide](https://deeplink-org.github.io/pvisor/en/start/installation/)
for platform requirements and executor setup.

## Run it

```bash
pvisor run --safe -- claude
pvisor status --review last
pvisor apply last --path src   # or: pvisor drop last
```

Replace the command after `--` with your script or installed Agent CLI. `--safe`
keeps workspace changes in Job storage for review, and `last` addresses the most
recent Job in the default storage. To put the stage elsewhere, use `--stage PATH`
and address later commands with that path or the printed Job ID (`run-*`) instead
of `last`.

## Bounded, recoverable, checkable

| Property | Meaning | Mechanism today |
|---|---|---|
| Bounded | the blast radius is known up front | executor boundary, capability admission |
| Recoverable | staged changes can be selectively merged, discarded, or forked before apply | staging, `apply`/`drop`, `fork` |
| Checkable | execution leaves checkable evidence | Run Bundle, evidence model, optional capture |

## Where we are today

pVisor focuses on bounded, recoverable, checkable execution and high sandbox
density on a single machine. Native Jobs retain the run-review-apply workflow;
the separate `pvisor-daemon` provides a partial OpenSandbox 1.1.0 API profile
through a VM-only `NativeRuntime` that embeds pVisor in detached supervisor
subprocesses. The runtime is implemented for Linux x86_64/KVM with delegated
cgroup v2, with the daemon CLI integrated. Prepared-image bootstrap
is neither supplied nor end-to-end validated. The daemon API does not implement
stage/apply or checkpoints, does not automatically acquire node sharing resources,
and has no SDK-conformance or density evidence.
Cross-node placement, workflows and retries belong to external orchestrators
such as Kubernetes or Ray, not a pVisor Cluster control plane.

For the four levels, where pVisor places its bet, and the status of L2/L3, see the
[trust ladder](https://deeplink-org.github.io/pvisor/en/why/trust-ladder/).

## Scope

A declared policy is not proof of enforcement, and `apply`/`drop` govern staged
files only, with no undo for remote API calls, database writes, or messages
already sent. Host, container, and VM executors differ, and container/VM support
is platform-dependent. See [capabilities and evidence](https://deeplink-org.github.io/pvisor/en/concepts/capabilities-and-evidence/).

## Documentation

- [Start here](https://deeplink-org.github.io/pvisor/en/start/) — the path from install to a reviewed run
- [Your first run](https://deeplink-org.github.io/pvisor/en/start/first-run/) — the run-review-apply loop
- [Why pVisor](https://deeplink-org.github.io/pvisor/en/why/) — vision, trust ladder, use cases, and comparisons
- [Capabilities and evidence](https://deeplink-org.github.io/pvisor/en/concepts/capabilities-and-evidence/) — how far a guarantee goes
- [Security](https://deeplink-org.github.io/pvisor/en/security/) — threat model, executor boundaries, and known limitations
- [Benchmarks](https://deeplink-org.github.io/pvisor/en/benchmarks/) — methods and current data
- [Design and research](https://deeplink-org.github.io/pvisor/en/design/) — architecture, isolation, and research directions
- [Community](https://deeplink-org.github.io/pvisor/en/community/) — contributing, testing, and roadmap
- [中文文档](https://deeplink-org.github.io/pvisor/zh/start/)

Security issues: see [SECURITY.md](SECURITY.md). Contributions: see [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[Apache License 2.0](LICENSE). See [`NOTICE`](NOTICE) for third-party
attributions and separately licensed bundled components.
