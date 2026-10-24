# pVisor overview {#overview}

<span id="what-is-policyvisor"></span>

**PolicyVisor (pVisor) is an agent-native sandbox framework that provides file-level oversight of access and modifications, sandbox execution lifecycle management, and image and memory resource optimizations. Together, these capabilities support secure, controlled, high-density agent execution.**

**Agent-Native:** pVisor integrates file-level oversight, sandbox lifecycle management, and resource reuse throughout agent execution. It uses files as the unit of management to bound access and modifications, isolate candidate changes, and support review; coordinates sandbox pause, memory offload, and resume through Host-side I/O management to actively reclaim resident memory from idle sandboxes during model waits; and uses on-demand image loading, memory sharing, and compression to reduce redundant resource use during branching exploration and rollback. Together, these mechanisms support secure, controlled, high-density agent execution.

<span id="responsibilities-of-the-execution-layer"></span>

## pVisor's place in an agent system {#system-role}

pVisor integrates agent-oriented management into sandbox execution: it oversees file and network interactions, coordinates sandbox lifecycles, and optimizes image and memory resources on the Host. The VM executor example below shows pVisor as a module within the sandbox execution framework.

![Agent above a Host sandbox framework containing pVisor and its runtime, with the guest VM below](../assets/diagrams/pvisor/agent-host-guest-en.svg)

The sandbox runtime / VMM runs the VM and its devices. pVisor coordinates the runtime through lifecycle and resource-management integration points, while Host-side [OverlayCore](../design/overlayfs.md) and [OverlayNet](../design/overlaynet.md) manage guest file and network I/O. These module relationships require corresponding runtime integration points; support for automatic waiting management is described in the [experiment record](../design/experimental/vcpu-idle-offload/index.md). The Agent or upper-level system owns decisions, result acceptance, reward computation, and cross-node scheduling.

<span id="decouple-supervision-from-execution-volume"></span>
<span id="reduce-intervention-during-execution"></span>
<span id="task-flow"></span>
<span id="when-to-use"></span>

## Use cases {#workflows}

**Local agent workflows**

To limit an agent's impact on your workspace and review and accept file changes after execution, start with [your first run](../start/first-run.md) and work through execution, review, and apply.

**Agent execution infrastructure**

To manage multiple execution environments for inference, evaluation, or post-training and reduce resource use during model waits and branching exploration, start with the [infrastructure overview](../guides/infrastructure/index.md) to choose an integration interface and runtime configuration.

<span id="trust-ladder"></span>

!!! note "Support scope"
    Isolating file changes requires [staging](../concepts/staging.md), which does not undo external API calls, database writes, or other external side effects. Isolation and resource management depend on the [executor](../security/executor-boundaries.md), [interface](../guides/infrastructure/integration.md#capabilities), and experimental status; see the corresponding [benchmarks](../benchmarks/index.md) for performance evidence.
