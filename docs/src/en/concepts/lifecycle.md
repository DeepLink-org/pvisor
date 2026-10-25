# Sandbox lifecycle management

While an Agent waits for external responses or data, its CPU may be idle while memory holding task context remains resident. On supported paths, pVisor handles waiting I/O on the Host and combines it with guest idle information to manage pausing, memory offload and resumption as the Agent task progresses.

<a id="agent-native"></a>

## How the Host handles waiting I/O {#host-management}

In the VM path, OverlayCore and OverlayNet connect file and network interactions to the Host, while the Runtime executes vCPUs and devices. The Host can continue some backend work while the guest is paused:

| Wait scenario | Work the Host can continue |
| --- | --- |
| **Model response** | Await the model response associated with this execution and collect bounded response data in supported configurations. |
| **Network download** | Receive data on supported, authorized download paths, maintain backpressure and prepare delivery to the guest. |
| **Lazy-image file read** | Fetch and verify file contents on demand through supported read-only paths, holding the result in Host buffers. |

The three scenarios provide different wait signals and share the Pause, Offload and Resume operations below. Pausing requires guest idle information, coordinated device access to memory and wake conditions; a pending request alone is insufficient. Data readiness or another wake event resumes the guest to continue tool execution. See [Automatic wait management: design and validation](../design/experimental/vcpu-idle-offload/index.md) for each path's enablement conditions.

## How Pause, Offload and Resume work together {#live-lifecycle}

| Operation | Effect |
| --- | --- |
| **Pause: suspend execution** | Stops vCPUs; guest RAM normally remains resident. |
| **Offload: reclaim resident memory** | After execution and device access are quiescent, synchronizes RAM to backing storage and requests reclamation of resident pages. |
| **Resume: continue execution** | Restores execution and device access; offloaded pages may fault back in on later access. |

Depending on the wait, choose between two paths:

- **Pause execution only:** running → Pause → Resume.
- **Also reclaim memory:** running → pause and satisfy offload conditions → Offload → Resume.

Offload is optional; the decision depends on wait duration and the cost of offloading and recovery. All three operations act on the same live VM and require the original owner/VMM and a reliable memory recovery source. See [Memory offload](../design/memory-optimization/offload.md).

<a id="checkpoint-boundary"></a>

Live Resume continues the original live instance. See [VM execution checkpoints](resource-reuse.md#execution-checkpoints) for the semantics of rebuilding an instance from saved state.

<a id="prerequisites"></a>
<a id="start-task"></a>
<a id="pause-resume"></a>
<a id="offload-resume"></a>
<a id="collect-results"></a>

## Further reading {#cli-example}

- [Pause, offload and resume a VM](../guides/infrastructure/lifecycle.md): CLI walkthrough.
- <a id="availability"></a>[Integration and capability matrix](../guides/infrastructure/integration.md#capabilities): lifecycle operations available through each interface.
- [Image and memory optimization](resource-optimization.md#lifecycle-and-cost): coordination between lifecycle management and resource optimization.
