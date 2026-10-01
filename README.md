# <img src="docs/src/assets/logos/pvisor-icon.png" alt="PolicyVisor" width="72" /> PolicyVisor (pVisor)

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

![PolicyVisor execution and review workflow](docs/src/assets/diagrams/pvisor/system-products.svg)

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
pvisor run --safe --stage ../stage-001 -- claude
pvisor status --review ../stage-001
pvisor apply ../stage-001 --path src   # or: pvisor drop ../stage-001
```

Replace the command after `--` with your script or installed Agent CLI. `--safe`
retains workspace changes in Job storage for review; `--stage PATH` selects
another location. With `--stage PATH`, address later commands with that path or
the printed Job ID (`run-*`): `last` only resolves Runs in the default storage,
so do not rely on it across projects.

## Bounded, recoverable, checkable

| Property | Meaning | Mechanism today |
|---|---|---|
| Bounded | the blast radius is known up front | executor boundary, capability admission |
| Recoverable | staged changes can be selectively merged, discarded, or forked before apply | staging, `apply`/`drop`, `fork` |
| Checkable | execution leaves checkable evidence | Run Bundle, evidence model, optional capture |

## Where we are today

| Level | How you intervene | Carried by | Status |
|---|---|---|---|
| L0 | Approve every command | one interactive session | Built into most agents |
| L1 | Review every change after the fact | one Job at a time, on your machine | **pVisor today** |
| L2 | Policy and evidence decide what can skip review; you spot-check | many Jobs, one pipeline | Next |
| L3 | Post-hoc audit; humans handle exceptions | clustered execution: cross-node scheduling, centralized evidence | Direction |

The vision is L3. Today pVisor runs one Job at a time on your machine, already
bounded, recoverable, and checkable—the properties the higher levels build on.
pVisor is the semantic layer for each execution; it does not replace schedulers
like Kubernetes or Ray.

## Scope

A declared policy is not proof of enforcement, and `apply`/`drop` govern staged
files only, with no undo for remote API calls, database writes, or messages
already sent. Host, container, and VM executors differ, and container/VM support
is platform-dependent. See [capabilities and evidence](https://deeplink-org.github.io/pvisor/zh/concepts/capabilities-and-evidence/).

## Documentation

- [Start here](https://deeplink-org.github.io/pvisor/en/start/) — the path from install to a reviewed run
- [Your first run](https://deeplink-org.github.io/pvisor/en/start/first-run/) — the run-review-apply loop
- [Capabilities and evidence (中文)](https://deeplink-org.github.io/pvisor/zh/concepts/capabilities-and-evidence/) — how far a guarantee goes
- [Project architecture (中文)](https://deeplink-org.github.io/pvisor/zh/design/) — ownership and delivery boundaries
- [中文文档](https://deeplink-org.github.io/pvisor/zh/start/)

## License

[Apache License 2.0](LICENSE). See [`NOTICE`](NOTICE) for third-party
attributions and separately licensed bundled components.
