# pVisor selection and usage links {#selection}

<span id="should-you-use-pvisor"></span>

Use pVisor for local controlled execution, reviewable changes, or SDK-driven local VM sandboxes. Choose by the interface you need:

- **Review workspace changes before merging:** use native Job staging, conflict checks and selective apply.
- **Reuse Docker command scripts:** check the experimental CLI subset and per-container VM differences.
- **Reuse OpenSandbox/E2B SDK applications:** check the daemon's tested core profile, Linux/KVM prerequisites and unsupported APIs.
- **Require a managed cloud service or hostile multi-tenant assurance:** assess service operations, security audits and isolation separately; the local interfaces do not provide those guarantees.

Remote API calls, database writes and sent messages retain their effects; [staging](../concepts/staging.md) defines the reversible file scope.

Adoption guidance is now part of the [pVisor overview](index.md#when-to-use). New users can start with the [demonstration](../start/first-run.md) or [infrastructure integration guide](../guides/infrastructure/index.md). This address remains available for existing links.

<span id="comparing-existing-approaches"></span>

## Selection evidence {#comparison-evidence}

For runtime costs and execution choices, see [end-to-end tasks](../benchmarks/agent-tasks.md), [reinforcement-learning execution environments](../benchmarks/compare-rl-infra.md), and [executor boundaries](../security/executor-boundaries.md). See the [integration matrix](../guides/infrastructure/integration.md) for interface support.

## Comparing Anthropic Sandbox Runtime {#sandbox-runtime}

Consider SRT when you want to retain the local toolchain while restricting a command or MCP Server's access. When you need to review execution results and choose which file changes to accept, pVisor supplies staging and Job records.

This comparison uses a source review on 2026-10-25: SRT [`c66fb77`](https://github.com/anthropics/sandbox-runtime/tree/c66fb77b28fdbea8b9788aca72377238ae67ca20) (main-branch package version `0.0.81`) and pVisor `b1d8d0c`. No matched performance or attack tests were run.

| Dimension | SRT | pVisor |
| --- | --- | --- |
| Selective Linux host egress | A network namespace isolates direct networking; Unix sockets bridge to host proxies. Clients ignoring proxies usually fail to connect | Selective proxying remains cooperative; enforced paths require a VM or host deny-all mode |
| File changes | Direct writes to allowed paths | With staging enabled: review, selective apply, conflict checks and drop |
| HTTPS and credentials | Configurable TLS termination, request filtering, credential sentinels and egress injection | Gateway focuses on LLM protocols, provider credential routing and trajectory capture |
| State and recovery | In-process proxy/configuration management and violation records; the embedder supplies persistence | Durable Jobs, Run Bundles and conditional checkpoint/restore support |
| Integration | npm CLI and TypeScript library, plus a standalone proxy interface | Native CLI, Rust runtime and single-node daemon; running an Agent CLI needs no language SDK |

See SRT's [Linux isolation implementation](https://github.com/anthropics/sandbox-runtime/blob/c66fb77b28fdbea8b9788aca72377238ae67ca20/src/sandbox/linux-sandbox-utils.ts), [network and credential configuration](https://github.com/anthropics/sandbox-runtime/blob/c66fb77b28fdbea8b9788aca72377238ae67ca20/src/sandbox/sandbox-config.ts) and [violation store](https://github.com/anthropics/sandbox-runtime/blob/c66fb77b28fdbea8b9788aca72377238ae67ca20/src/sandbox/sandbox-violation-store.ts). Capabilities depend on configuration and platform; for example, SRT's default read rules are empty, so sensitive paths need explicit protection.

pVisor host and VM executors provide different controls; see [network boundaries](../guides/policies/network.md#网络边界) for enforcement strength. Staging does not strengthen network isolation, and a VM does not automatically hide explicitly exposed host files. Cost comparisons should match the platform, policy and task, accounting separately for staging and VM semantics. Source size and implementation language do not establish superior performance or security.

<span id="faq"></span>

## Usage help {#usage-help}

- Connecting agents or CI: [agent integration](../guides/agents/index.md) and [CI workflows](../guides/ci.md).
- Staging, accepting changes, and locating Jobs: [staging and apply](../concepts/staging.md) and [Jobs and storage](../concepts/jobs.md).
- Network access and data boundaries: [security overview](../security/index.md) and [network policies](../guides/policies/network.md).
- Startup and file-operation overhead: [startup latency](../benchmarks/startup.md) and [filesystem tasks](../benchmarks/filesystem.md).

pVisor does not collect or upload usage statistics. Agent model requests, image downloads, remote caches and Gateway upstream requests access the network according to configuration. Whether the `--safe` preset's model-destination rules are non-bypassable depends on the executor; direct connections can still bypass selective Linux host proxying. See [network boundaries](../guides/policies/network.md#网络边界).
