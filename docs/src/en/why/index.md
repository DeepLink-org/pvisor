---
description: What pVisor is, where it sits in an agent system, the local and infrastructure workflows it supports, and the supervision levels available today.
---

# Introduction {#overview}

<span id="what-is-policyvisor"></span>

pVisor provides controlled execution, file-change delivery and checkable records for agents on one machine. Use native Jobs to run and review tasks, Docker-style commands for independent Linux VMs, or OpenSandbox/E2B SDKs against a local daemon. Shared images and optional memory mechanisms reduce duplicate resources; measure the benefit for each workload and platform.

**PolicyVisor (pVisor) is an agent-native sandbox framework that provides file-level oversight of access and modifications, sandbox execution lifecycle management, and image and memory resource optimizations. Together, these capabilities support secure, controlled, high-density agent execution.**

**Agent-Native:** pVisor integrates file-level oversight, sandbox lifecycle management, and resource reuse throughout agent execution. It uses files as the unit of management to bound access and modifications, isolate candidate changes, and support review; coordinates sandbox pause, memory offload, and resume through Host-side I/O management to actively reclaim resident memory from idle sandboxes during model waits; and uses on-demand image loading, memory sharing, and compression to reduce redundant resource use during branching exploration and rollback. Together, these mechanisms support secure, controlled, high-density agent execution.

<span id="responsibilities-of-the-execution-layer"></span>

## pVisor's place in an agent system {#system-role}

pVisor integrates agent-oriented management into sandbox execution: it oversees file and network interactions, coordinates sandbox lifecycles, and optimizes image and memory resources on the Host. The VM executor example below shows pVisor as a module within the sandbox execution framework.

![Agent above a Host sandbox framework containing pVisor and its runtime, with the guest VM below](../assets/diagrams/pvisor/agent-host-guest-en.svg)

The sandbox runtime / VMM runs the VM and its devices. pVisor coordinates the runtime through lifecycle and resource-management integration points, while Host-side [OverlayCore](../design/overlayfs.md) and [OverlayNet](../design/overlaynet.md) manage guest file and network I/O. These module relationships require corresponding runtime integration points; support for automatic waiting management is described in the [experiment record](../design/experimental/vcpu-idle-offload/index.md). The Agent or upper-level system owns decisions, result acceptance, reward computation, and cross-node scheduling.

<span id="decouple-supervision-from-execution-volume"></span>
<span id="reduce-intervention-during-execution"></span>
## Entry points and scope {#interfaces}

Native Job executors install controls through host namespaces/Seatbelt, an OCI runtime or a VM; the runtime owns lifecycle, staging and records. Two environment-oriented entry points complement Jobs: the Docker-style CLI manages per-container VMs, and the daemon provides local VM lifecycle, command and file interfaces for tested OpenSandbox/E2B protocol subsets.

Job stage/apply, checkpoints and Gateway capture have separate contracts and cannot be invoked with sandbox IDs. Choose an interface and platform using the [CLI](../reference/cli.md) and [platform matrix](../reference/platforms.md). External orchestrators own cross-node placement, business retries and workflows.

<span id="task-flow"></span>
<span id="when-to-use"></span>

## Use cases {#workflows}

**Local agent workflows**

To limit an agent's impact on your workspace and review and accept file changes after execution, start with [your first run](../start/first-run.md) and work through execution, review, and apply.

**Agent execution infrastructure**

To manage multiple execution environments for inference, evaluation, or post-training and reduce resource use during model waits and branching exploration, start with the [infrastructure overview](../guides/infrastructure/index.md) to choose an integration interface and runtime configuration.

## Today, and after {#trust-ladder}

Supervision and deployment scale are independent dimensions: multiple Jobs can run concurrently and SDKs can manage several sandboxes. This alone does not establish exemption from review, batch merging or distributed scheduling.

| Level | Human intervention | Current status |
| --- | --- | --- |
| L0 | Approve commands before execution | Use agent-native approval or the Job `--ask` channel |
| L1 | Review changes after execution, then select what to merge | Native Jobs provide stage, review, apply/drop and conflict checks |
| L2 | Policy and evidence determine exemptions; humans spot-check | Callers can compose existing checks; a unified exemption policy and automatic merge gate still need acceptance |
| L3 | Continuous audit; humans handle exceptions | Evidence aggregation and external orchestration remain integration directions without a complete product contract |

Native Job review, independent-workspace concurrency examples and CI integration are available today; see [parallel agents](../guides/parallel-agents.md) and [CI](../guides/ci.md). The [daemon](../guides/daemon/index.md) SDK interfaces manage sandbox lifecycles and process/file operations; they do not inherit the Job file-merge workflow.

[RL rollouts](../guides/rl-rollouts.md) can reuse recording, forking and tool-prefix replay. Training-framework integration and end-to-end throughput require separate acceptance.

!!! note "Automation gates and capacity evidence"
    Supervision cost per unit of agent work remains unmeasured; see [supervision cost](../benchmarks/supervision-cost.md). The [roadmap](../community/roadmap.md) tracks automation-gate gaps. Size deployments using the specific configurations and evidence in [concurrency density](../benchmarks/density.md).

!!! note "Support scope"
    Isolating file changes requires [staging](../concepts/staging.md), which does not undo external API calls, database writes, or other external side effects. Isolation and resource management depend on the [executor](../security/executor-boundaries.md), [interface](../guides/infrastructure/integration.md#capabilities), and experimental status; see the corresponding [benchmarks](../benchmarks/index.md) for performance evidence.
