# Local sandbox admission

Bound accepted sandbox resources on one machine before native VM creation. The daemon admits hard-limit sums; it does not queue work, choose hosts or infer spare capacity from a low RSS sample.

## Request validation {#model}

Creation requires a local prepared-image key in `image.uri`, nonempty argv and `resourceLimits` containing exactly `cpu` and `memory`. CPU accepts whole/fractional cores or millicores; memory accepts bytes and supported decimal/binary suffixes. Quantities must be positive, exactly representable and checked for overflow.

```json
{
  "image": {"uri": "prepared-agent-1.1.0"},
  "entrypoint": ["sleep", "600"],
  "resourceLimits": {"cpu": "500m", "memory": "256Mi"},
  "timeout": 600,
  "metadata": {"purpose": "local-example"}
}
```

This is a request-shape example, not a validated image recipe. The manifest `entrypoint` bootstrap must supervise these arguments and the required services; [operations](operations.md#image-contract) defines that contract. No image is pulled during creation.

Optional platform selection must match the native Linux architecture. Unsupported snapshots/templates, resource requests, network policy, credential proxy, secure access, volumes, image authentication, lifecycle hooks and nonempty extensions fail before native creation. Environment and metadata maps are bounded and validated.

## Capacity accounting {#admission}

| Bound | Admission rule |
| --- | --- |
| Sandbox count | Registry record count must remain within `max_sandboxes` |
| CPU | Sum of all records' `cpu_millis` plus the request must fit configured capacity |
| Memory | Sum of all records' `memory_bytes` plus the request must fit configured capacity |

The serialized durable insertion is the reservation barrier. Concurrent creations cannot both spend the same capacity. Refusal returns `429 CAPACITY_EXCEEDED`, not an assignment for later execution. There are no tenant quotas or authenticated per-tenant resource accounts.

Paused, terminated, failed and uncertain records retain their full CPU/memory/count charge until explicitly deleted or failed creation has verified cleanup. Pause does **not** release logical CPU. Native deletion and absence confirmation precede durable record removal; unknown cleanup cannot create reusable capacity.

## Installed controls and physical memory {#reservations}

Requires Linux x86_64, usable `/dev/kvm`, trusted absolute paths and a writable delegated cgroup v2 hierarchy with enabled CPU/memory/PID controllers and `cgroup.kill`. Preflight checks real controller writes and the KVM API; there is no host, OCI command or registry-pull fallback.

The native supervisor embeds `pvisor::PVisor` with only `VmExecutor` and holds its RunHandle in a detached subprocess. The sandbox cgroup caps the entire supervisor/VMM/helper tree: aggregate CPU rate **10–8000 millicores**, hard memory, zero swap, `pids.max=512` and group OOM. vCPU count rounds quota up to whole CPUs (at most 8); guest RAM rounds down to MiB, while the hard memory cap also includes host overhead. Controls are rechecked during lifecycle/endpoint observations. Paused, failed and uncertain records retain conservative admission charges.

The launch callback joins the identity-bound cgroup **before exec**, using a pre-opened `cgroup.procs` FD under the owner lock and checking started/deletion/tombstone markers. Supervisor startup verifies membership instead of moving an already-running Tokio process; supervisor/Tokio allocations and subsequent VM/helper children are charged inside the sandbox budget. Direct hidden-command invocation outside that cgroup fails closed.

Logical admitted sums, installed supervisor/VM-tree limits and whole-node physical occupancy are different quantities. Daemon, helpers, caches and the host need headroom outside sandbox limits. There is no implicit CPU/RAM overcommit, pressure-based admission or measured node-wide physical budget. Native [shared working sets](shared-working-set.md) do not authorize lower daemon reservations.

Validation and arithmetic live in `daemon/models.rs`; serialized reservation insertion lives in `daemon/mod.rs`; native KVM/cgroup preflight and controls live in `runtime.rs`.
