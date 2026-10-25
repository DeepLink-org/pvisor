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

<span id="faq"></span>

## Usage help {#usage-help}

- Connecting agents or CI: [agent integration](../guides/agents/index.md) and [CI workflows](../guides/ci.md).
- Staging, accepting changes, and locating Jobs: [staging and apply](../concepts/staging.md) and [Jobs and storage](../concepts/jobs.md).
- Network access and data boundaries: [security overview](../security/index.md) and [network policies](../guides/policies/network.md).
- Startup and file-operation overhead: [startup latency](../benchmarks/startup.md) and [filesystem tasks](../benchmarks/filesystem.md).
