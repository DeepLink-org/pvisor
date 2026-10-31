---
description: The two modes of the pVisor sandbox framework, their isolation mechanisms and result-acceptance boundaries.
---

# Introduction {#overview}

<span id="what-is-policyvisor"></span>

PolicyVisor (pVisor) is a sandbox framework with two modes. Docker style sandbox runs familiar container commands in isolated VMs. Agent style stagebox is pVisor's original mode for coding agents, with staged file changes that you review before accepting.

## Two modes {#modes}

| Mode | Entry points | Isolation and result management |
| --- | --- | --- |
| Docker style sandbox | Container commands such as `pvisor run`, `exec`, `stop`, and `rm` | A separate Linux VM for each container; container lifecycle, with a separate staged-bind review/apply/drop contract on Linux. |
| Agent style stagebox | Start a native Job with `pvisor spawn --safe`, then review and apply/drop | Retain candidate files before selective acceptance; isolation depends on the Host, OCI, or VM executor and installed controls. |

Sandbox names the framework and execution isolation. Stagebox refers specifically to the Agent style mode. A stage stores file changes awaiting acceptance. Stagebox does not name a new VM or isolation level, and does not automatically disable agent approvals.

Docker style sandbox currently provides an experimental Docker-compatible command subset, without Docker Engine API or Compose; see [container commands](../reference/cli.md#container-commands). Start Agent style stagebox with the [offline tutorial](../start/first-run.md), then configure authentication and networking using the [agent guides](../guides/agents/index.md). Ordinary Host Jobs write through by default; file review and acceptance require explicit staging.

“Hands-off vibe coding. Coding without sandbox approvals.” describes one feature of Agent style stagebox within the sandbox framework. A verified agent/version/configuration matrix for approval-free operation is not yet published. Initial authentication, external-write controls, and final result review still need explicit handling.

The framework also manages VM lifecycles, images, and memory for inference, evaluation, and RL post-training integrations. Capabilities depend on the interface, executor, and platform; capacity benefits require workload-specific measurements.

<span id="responsibilities-of-the-execution-layer"></span>

## pVisor's place in an agent system {#system-role}

pVisor integrates agent-oriented management into sandbox execution: it oversees file and network interactions, coordinates sandbox lifecycles, and optimizes image and memory resources on the Host. The VM executor example below shows pVisor as a module within the sandbox execution framework.

![Agent above a Host sandbox framework containing pVisor and its runtime, with the guest VM below](../assets/diagrams/pvisor/agent-host-guest-en.svg)

The sandbox runtime / VMM runs the VM and its devices. pVisor coordinates the runtime through lifecycle and resource-management integration points, while Host-side [OverlayCore](../design/overlayfs.md) and [OverlayNet](../design/overlaynet.md) manage guest file and network I/O. These module relationships require corresponding runtime integration points; support for automatic waiting management is described in the [experiment record](../design/experimental/vcpu-idle-offload/index.md). The Agent or upper-level system owns decisions, result acceptance, reward computation, and cross-node scheduling.

<span id="decouple-supervision-from-execution-volume"></span>
<span id="reduce-intervention-during-execution"></span>
## Entry points and scope {#interfaces}

Native Job executors install controls through host namespaces/Seatbelt, an OCI runtime or a VM; the runtime owns lifecycle, staging and records. Two environment-oriented entry points complement Jobs: the Docker-style CLI manages per-container VMs, and the daemon provides local VM lifecycle, command and file interfaces for tested OpenSandbox/E2B protocol subsets.

Native Job operations and daemon sandbox IDs use separate contracts. Container staged binds expose selected review/apply/drop operations with their own source and mount selectors. Choose an interface and platform using the [CLI](../reference/cli.md) and [platform matrix](../reference/platforms.md). External orchestrators own cross-node placement, business retries and workflows.

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
