# Core concepts {#job}

pVisor adds management capabilities tailored to Agent tasks to sandboxes, managing access boundaries, execution lifecycles, task state, and resources on the Host. Agent commands and tools execute inside the sandbox; file and network interfaces connect interactions within and outside the environment; task records associate execution with its results; and state and resource management supports continued execution and multiple attempts.

## System architecture {#objects}

### Task origin {#task-origin}

- **Agent**: A program that decides its next action based on task goals, model responses, and execution feedback, using tool calls to request command execution, file operations, and other actions. It may run inside the sandbox or invoke it from outside.
- **Integration options**: Agents or higher-level systems use pVisor through the CLI, Rust Runtime, or service APIs to submit execution requests and obtain results. Supported capabilities and managed objects vary by entry point; see the [integration matrix](../guides/infrastructure/integration.md) to choose an option.

### Host and management {#host-management}

- **Host**: The system running pVisor and its execution backend, providing compute, memory, storage, and network resources for sandboxes. In VM deployments, the Host is distinct from the Guest VM in which tools run.
- **pVisor management module**: Provides management capabilities for Agent tasks on the Host, overseeing file and network interactions through OverlayCore and OverlayNet and integrating with sandbox lifecycle and resource management.
- **Executor**: pVisor's common abstraction for different execution backends, routing execution requests to Host processes, OCI containers, or VMs. Available isolation and management capabilities depend on the backend and configuration.
- **Runtime/VMM**: The underlying execution component that an executor integrates with. For example, an OCI container runtime runs containers, while a VMM manages virtual CPUs, memory mappings, and virtual devices.

### Execution environment {#execution-environment}

An execution environment is where Agent commands and tools actually run, including the files, software dependencies, processes, and network access available to them. Depending on the execution backend, it can take the following forms:

- **Host process**: Commands run directly as processes on the host system, with access governed by configured policies and host control mechanisms.
- **OCI container**: Commands run inside a container, using its filesystem and process environment while sharing the Host kernel.
- **Guest VM**: Commands run inside a virtual machine with an independent guest kernel and allocated virtual CPUs, memory, and devices.

Isolation and management capabilities vary across execution environments. See [Executor boundaries](../security/executor-boundaries.md) for the supported scope.

The Agent initiates tool calls; pVisor and the execution backend on the Host oversee access and manage execution, while commands and tools run in the selected execution environment.

## File views and network interactions {#file-network}

### File views and candidate changes {#file-state}

- **Workspace**: The set of files where a task reads inputs, makes edits, and delivers results. With staging enabled, the target workspace is where accepted changes are ultimately applied.
- **File view**: The file tree visible to the execution environment. With staging enabled, it combines the lower and private upper layers so tools can read both original files and their own changes.
- **Lower layer**: Supplies base content for the file view, from either an external workspace or a managed immutable base. Other programs may still modify an external workspace.
- **Private upper layer**: With staging enabled, retains the task's additions, modifications, and deletions, isolating candidate changes from the target workspace.
- **Changeset**: A reviewable list of net additions, modifications, and deletions, rather than a log of every file operation.

### Handling file changes {#handling-changes}

- **Stage**: The staging mechanism that retains file changes in the private upper layer for later review, without writing them to the target workspace.
- **Apply**: Writes selected, reviewed candidate changes to the target workspace.
- **Drop**: Discards candidate changes that have not been accepted. It does not undo changes already applied.

With staging enabled, the Agent reads inputs and leaves candidate changes through the file view. After review, the caller uses Apply to accept the desired changes or Drop to discard the remainder. See [File-level oversight and isolation](staging.md) for mechanisms and boundaries, and [Review and apply changes](../guides/review-apply.md) for concrete operations.

### Network access and access policies {#network-policy}

- **Egress**: The path through which an execution environment initiates connections to external services, such as model APIs, dependency downloads, or application services.
- **Policy**: Defines the allowed scope of file and network access, such as readable or writable paths and permitted network destinations.

File policies constrain file access, while network policies constrain outbound connections. Actual coverage depends on the execution backend and integrated control mechanisms. Discarding file changes does not undo external effects caused by network requests.

### File and network management components {#access-components}

- **OverlayCore**: Manages file views, copy-on-write, and candidate changes, providing the foundation for file-level oversight and review.
- **OverlayNet**: Manages network interactions that pass through its control paths, checking and handling outbound connections according to network policies.

Access outside the file view still depends on executor boundaries. See [Security and support scope](../security/index.md) for control coverage.

## Tasks, executions, and records {#task-records}

- **Job**: A unit of work managed persistently in pVisor's native workflow, associating a command and execution records with candidate changes when staging is enabled. After the command finishes, the same Job can still be used to query results, review file changes, and decide how to handle them.
- **Attempt**: One concrete execution of a Job, distinguishing separate executions. A single Attempt can run an Agent program that makes multiple rounds of tool calls.
- **Run Bundle**: A record package combining task and execution identities, exit status, output, evidence of controls actually in effect, and file changes when staging is enabled, for review, diagnosis, and consumption by higher-level systems.

A Job identifies the work, an Attempt distinguishes a particular execution, and a Run Bundle brings together execution information for inspection. Restoring a supported full execution checkpoint preserves the Job identity and creates a new Attempt; forking to explore an alternative creates a new Job and preserves its lineage.

Completing execution does not mean accepting file changes: the caller decides whether to Apply them after review.

<span id="job-storage"></span>
<span id="_1"></span>
<span id="_2"></span>
<span id="_3"></span>
<span id="finding-it-again-later"></span>
<span id="jobs-and-storage"></span>
<span id="retention-and-cleanup"></span>
<span id="storage-location"></span>

See [Job usage and storage](../reference/jobs.md) for usage and [Run Bundle format](../reference/run-bundle.md) for record fields.

<span id="interfaces"></span>

These concepts describe the native Job workflow. Standalone containers and daemon sandboxes have their own identities and interfaces; see the [integration matrix](../guides/infrastructure/integration.md) for supported capabilities.

## State and resources {#state-resources}

### State preservation and branching {#saved-state}

- **State**: File content needed to continue execution and, where applicable, CPU, memory, and device state.
- **Checkpoint**: A consistent saved state within a specified scope. Workspace checkpoints preserve file changes and related metadata; full execution checkpoints also preserve machine state under supported configurations.
- **Restore**: Reconstructing the corresponding file or execution environment from a checkpoint. How execution can continue depends on the scope of state preserved.
- **Branch**: An independent continuation from the same saved state, keeping modifications from different attempts isolated.

The same checkpoint can be used for restoration or as the starting point for multiple independent branches. Checkpoint restoration does not roll back external services; see [State preservation and branch reuse](resource-reuse.md) for the semantics.

### Pausing, offloading, and resuming execution {#live-lifecycle}

- **Pause**: Pauses the original VM's vCPUs; memory usually remains resident.
- **Offload**: Once pause and device-access coordination requirements are met, synchronizes RAM to backing storage and requests reclamation of resident pages.
- **Resume**: Resumes execution of the original instance. Offloaded memory pages can be read back on subsequent access.

Pause, Offload, and Resume here manage the same live VM, retaining the original VMM and a reliable source for memory recovery. Reconstructing an execution environment from a checkpoint is Restore, as described above. The native CLI's `suspend` and `resume` commands use checkpoint restoration; see [Sandbox lifecycle management](lifecycle.md) for the distinctions and interface support.

### Image and memory resources {#resource-storage}

- **Image**: Base filesystem content used to prepare an execution environment, including system files, tools, and dependencies.
- **Shared base**: Immutable base content that multiple instances can reuse while keeping subsequent writes private. Lower describes the role of content in a file view; a shared base emphasizes reuse of immutable content across instances.
- **Resident memory**: Host physical memory currently in use, distinct from the guest RAM capacity allocated to a VM.
- **Backing storage**: A storage source that preserves memory or other state content for later reads. It is not, by itself, a full checkpoint.

Images and shared bases reduce repeated preparation. On-demand loading, sharing, and compression reduce resource use at their respective layers. See [Image and memory optimization](resource-optimization.md) for mechanisms, applicability, and costs.
