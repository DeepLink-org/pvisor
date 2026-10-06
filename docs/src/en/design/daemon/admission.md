# Local sandbox admission

Bound accepted sandbox resources on one machine before calling Podman. The daemon admits hard-limit sums; it does not queue work, choose hosts or infer spare capacity from a low RSS sample.

## Request validation {#model}

Creation requires an image URI, nonempty argv and `resourceLimits` containing exactly `cpu` and `memory`. CPU accepts whole/fractional cores or millicores; memory accepts bytes and supported decimal/binary suffixes. Quantities must be positive, exactly representable and checked for overflow.

```json
{
  "image": {"uri": "localhost/prepared-agent:1.1.0"},
  "entrypoint": ["sleep", "600"],
  "resourceLimits": {"cpu": "500m", "memory": "256Mi"},
  "timeout": 600,
  "metadata": {"purpose": "local-example"}
}
```

This is a request-shape example, not a validated image recipe. The image ENTRYPOINT must supervise these arguments and the required services; [operations](operations.md#image-contract) defines that contract. No image is pulled during creation.

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

The Linux backend requires rootless Podman, cgroup v2 and delegated CPU, memory and PID controllers. It installs CPU quota, hard memory and swap settings, PID limits, private namespaces, `no-new-privileges` and `cap-drop=ALL`, then checks resource settings in container configuration. Failed prerequisites have no rootful or host fallback.

Logical admitted sums, installed per-container limits and whole-node physical occupancy are different quantities. Daemon, helpers, caches and the host need headroom outside sandbox limits. There is no implicit CPU/RAM overcommit, pressure-based admission or measured node-wide physical budget. Native [shared working sets](shared-working-set.md) do not authorize lower daemon reservations.

Validation and arithmetic live in `daemon/models.rs`; serialized reservation insertion lives in `daemon/mod.rs`; Podman preflight and control configuration live in `runtime.rs`.
