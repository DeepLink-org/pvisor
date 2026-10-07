# Core mechanisms

pVisor's runtime organizes a task around four parts: its execution environment, access boundaries, lifecycle, and results. The caller supplies a command and policy; the runtime prepares resources, supervises execution, and retains a record. With staging enabled, the caller reviews and accepts file changes separately.

The local CLI and embedded callers share an execution path. The single-node daemon invokes native VM execution in detached supervisors and exposes a sandbox lifecycle API. External orchestration handles cross-node host selection, queues, and application retries.

## System model {#architecture}

```text
CLI / Embedded caller / Daemon supervisor
                    │
        Execution request and effective policy
                    │
          Session lifecycle management
                    │
       ┌────────────┼────────────┐
    Executor   File service   Network service
       └────────────┼────────────┘
                    │
       Results, observations, and records
                    │
       Caller validation and artifact handling
```

Executors provide process, container, or VM environments. File and network services enforce access policy along their respective data paths. Session coordinates these components into a managed execution and owns resource preparation, cancellation, cleanup, and terminal publication.

`pvisor-core` holds shared types and pure policy evaluation; `pvisor` and drivers implement runtime and platform mechanisms. Gateway, TUI, and Replay add model communication, interaction, and trajectory handling. See [Core architecture](architecture.md) for component ownership, host control protocols, and service integration details.

## How a task completes {#execution}

1. **Admission.** Check the request, executor capabilities, and platform prerequisites to produce effective policy and placement.
2. **Preparation.** Establish the file view, network access, and runtime resources, then commit required startup facts.
3. **Execution.** Start the command, receive cancellation and control requests through the run handle, and collect output and observations.
4. **Teardown.** Clean up managed resources, check controls, save records, and publish terminal state.
5. **Validation.** The caller checks execution results and, with staging enabled, decides which file changes to accept.

Execution completion, resource cleanup, and artifact acceptance each have an outcome. A failed command may leave useful files; cancellation requires waiting for stop and cleanup results; recording failure still calls for reconciling effects already performed.

A Job is the work item the user continues to operate on, an Attempt identifies one execution, an Operation describes a processing request, and an Event records an observed fact. See the [execution model](execution-model.md) for identity, state, and recovery relationships, and [Operation and Event](operations-events.md) for event chains and commit semantics.

## Access and file changes {#boundaries}

The file service combines a workspace baseline with task-private changes into an execution view. With staging enabled, writes stay in the private layer; apply checks conflicts and publishes selected paths to the workspace. Host FUSE and VM virtio-fs share file semantics, with each entry point handling its protocol and platform requirements. See [OverlayCore](overlayfs.md) for copy-on-write, conflict baselines, and partial-publication recovery.

The network service checks destinations and permissions at the proxy or VM data plane; Gateway can also handle model requests. Coverage depends on the executor and access path, so records retain requested policy, installed controls, and observations separately. See [OverlayNet](overlaynet.md) for network paths and [Isolation mechanisms](isolation.md) for protection scope across execution environments.

## State and resource reuse {#resources}

Images provide read-only environment inputs, while task writes remain private. The shared image cache pins content revisions and reads indexes and file blocks on demand, reducing the need to unpack a complete image beforehand. Small-file packing and read coalescing belong to the Lazy Image V2 proposal; see [Shared image cache](shared-image-cache-storage.md) for the current format and implementation.

Memory optimization treats duplicate content, cold working sets, and idle environments separately: sharing reduces copies, compression reduces retained size, and offload moves content to recoverable storage. Mechanism selection accounts for recovery sources, CPU, peak memory, and the next task's wait. See [Memory optimization](memory-optimization/index.md) for combinations and platform status.

## Retaining results {#results}

Execution records connect requests, effective policy, installed controls, exit results, and artifacts. Job metadata locates work, Bundles support result review, and Journal retains committed facts. The file layer manages candidate changes and conflict baselines. See [Journal](journal.md) for event writing, receipts, and recovery.

Recovery starts by identifying what was retained: file checkpoints preserve candidate files, agent trajectories preserve historical context, and execution checkpoints preserve machine state for supported profiles. The application coordinates remote API, database, and message effects. Callers use these distinctions to choose re-execution, environment restoration, or continued review.

## Key tradeoffs {#principles}

- **Common contracts with platform-specific mechanisms.** Callers use shared request and result models; drivers report supported capabilities and installed controls.
- **Central lifecycle ownership with separate data paths.** Session manages execution resources and terminal state; file, network, and VM components manage their respective access paths.
- **Separate execution from artifact publication.** Execution results describe what happened; file acceptance determines which changes reach the target workspace.

See [Core design principles](principles.md) for the development constraints, causal rules, and evidence requirements behind these choices.
