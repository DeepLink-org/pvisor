# Executor boundaries

This summarizes capability scope when corresponding controls are requested. [Network boundaries](../guides/policies/network.md#网络边界) owns network detail; [isolation](../design/isolation.md) owns mechanisms. Verify controls installed for each Run in its Bundle.

## Matrix

| Dimension | Linux host | macOS host | Container | VM |
| --- | --- | --- | --- | --- |
| Workspace writes | Staged via FUSE before apply | Staged via macFUSE | Staged via OverlayFS | virtio-fs copy-on-write |
| Other host writes | `--filesystem sandbox`: namespaces/projected root/Landlock | `--filesystem sandbox`: Seatbelt | Image and explicit mounts | Independent guest, temporary root upper |
| Reads | Ambient by default; sandbox confines | Ambient by default; safe requests Seatbelt, current evidence still reports ambient | Image and mounts | Rootfs and declared shares; `--rootfs host` exposes host data |
| Sensitive paths in view | `--access`/`--safe` OverlayFS rules | Same | Same | Same, including root view |
| Selective network | Cooperative, bypassable | Safe blocks direct connections; proxy filters | Cooperative with host network | Mandatory smoltcp |
| Offline | Private network namespace | Seatbelt external-IP/ambient-Unix block | `--container-network none` | `--overlaynet off` |
| HOME/credentials | `--safe` private HOME; explicit `--pass-env` grants | Safe temporary HOME | Explicit mounts/variables | Rootfs-dependent; host root preserves host HOME |
| Descendants | Namespaces and group cleanup | Group cleanup | Container process tree | Guest kernel |
| `--safe` usable? | Yes, prerequisites required | Yes, prerequisites required | **Rejected**: incomplete controls | Yes, auto required |

## Interpretation

- A control in one dimension does not establish others.
- No executor claims complete subprocess enforcement. `--strict` therefore rejects with UnsupportedPolicy on host/container/VM; it tests fail-closed behavior.
- macOS VM is not a hostile multi-tenant boundary.
- Detached descendants escape host process-group cleanup.

## Verify a Run

`pvisor status --review` displays installed controls and warnings; `--json` exposes structured fields. executor_observations is authoritative; safety.* is derived. See [control levels](../concepts/capabilities-and-evidence.md).
