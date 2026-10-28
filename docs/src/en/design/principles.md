# Core design principles

pVisor is designed around three questions: how to constrain Agent execution, how to manage state and resources throughout a task, and how to establish its results. These principles explain the rationale and boundaries of those designs. See the [architecture overview](index.md) for component responsibilities and execution flows.

## Separate shared contracts from execution implementations {#separate-definitions-from-execution}

Interfaces and executors should share consistent semantics for operations, policies and outcomes, while their implementations own platform-specific mechanisms. Core defines shared contracts; the pVisor Runtime and drivers implement execution. For example, Host and VM executors use shared operation and outcome contracts, but run tools through host processes and a guest respectively, and may support different controls. Contracts should expand with actual integration needs, avoiding premature abstractions for capabilities without callers. See [core architecture](architecture.md) for component responsibilities.

## Ground guarantees in actual execution boundaries {#guarantees-require-actual-boundaries}

A requested policy states constraints; guarantees depend on installed controls and the access paths they cover. For example, OverlayNet checks access rules at the VM network egress, while model messages inside ordinary TLS connections enter the model observation path only when integrated through Gateway. Accepting a policy or recording an access does not, by itself, establish enforcement. Capability descriptions must reflect the actual control boundaries of the executor and platform. See [capabilities and evidence](../concepts/capabilities-and-evidence.md) and [isolation design](isolation.md).

## Give cleanup and terminal publication one owner {#one-lifecycle-owner}

Whether execution finishes normally, is cancelled or fails partway through, an explicit owner must complete cleanup and publish its terminal state. Multiple components must not independently declare completion. Session owns the Attempt lifecycle; drivers report output and observations. For example, after a tool process exits, resource cleanup and required result persistence still need to finish before the task terminal state is published. Cancellation requests a stop rather than establishing completion, and execution failures must be reported separately from recording failures. See the [execution model](execution-model.md#attempt-completion) for completion handling.

## Match resource residency to execution needs {#resource-lifecycle}

Retaining an Agent's execution context does not require keeping all resources resident at all times. Sharing and demand loading reduce duplicate storage and premature preparation; lifecycle management determines when resources remain resident or are reclaimed. For example, on supported model-wait paths, the Host continues waiting for the response and uses guest idle evidence to manage VM pause, memory offload and resume. Reclamation must meet execution and device-access safety conditions and weigh wait length against recovery cost, rather than minimize resident memory alone. See [sandbox lifecycle management](../concepts/lifecycle.md) and [image and memory optimization](../concepts/resource-optimization.md) for the mechanisms.

## Separate execution changes from result acceptance {#review-files-before-applying-them}

Successful execution does not mean every file change should be accepted, and failed execution can leave useful changes. With staging enabled, candidate changes remain separate for the caller to review and selectively accept. For example, an Agent’s edits to `config.toml` stay in a private upper layer until the caller reviews the diff and uses Apply to write selected changes to the target workspace. This mechanism covers files within the staging scope; it cannot undo external side effects that have already occurred. See [file-level supervision and isolation](../concepts/staging.md) for operations and scope.

<a id="records-reconstruct-facts"></a>

## Keep intent, decisions and facts distinct in records {#separate-requests-decisions-and-facts}

Records should connect execution intent, actual decisions and observed results while preserving their distinct meanings. None can substitute for another. When requests are rewritten, both original and effective snapshots must be retained so that the final decision does not overwrite the original intent. For example, requesting access to a domain, having that access authorized, and successfully connecting are distinct facts; an authorization decision cannot substitute for the runtime outcome. The information categories and their associated objects are shown below:

![Distinguishing requests, policy decisions, installed controls, observations and result acceptance](assets/evidence-flow.svg)

Records make execution reviewable, but Events alone cannot deterministically replay side effects. See [Operation and Event](operations-events.md) for record semantics, and [replay design](replay.md) for the state, inputs and execution mechanisms replay requires.

## Preserve execution dependencies {#preserve-known-causal-dependencies}

An Agent's next operation may depend on an earlier output. Scheduling, forwarding and execution must preserve that known dependency even when placement or concurrency changes. For example, if an Agent edits a configuration and then runs a test that depends on it, the test must read the updated state rather than run early because of concurrent scheduling. Records must retain the corresponding causal references. Journal commit order and observation time cannot substitute for causality or establish a global order of effects across Jobs. See [Operation and Event](operations-events.md) and [Journal design](journal.md) for record fields and ordering semantics.
