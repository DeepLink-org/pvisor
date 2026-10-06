# Executor boundaries

Each executor provides the protection below per capability dimension. Every statement describes the case where **the corresponding control was requested**; whether a given Run actually installed it is decided by that Run's Bundle.

[Network boundaries](../guides/policies/network.md#网络边界) owns network detail (which path can be bypassed, and the VM data plane's protocol coverage); [isolation design](../design/isolation.md) owns the mechanisms.

## Overview

| Dimension | host (Linux) | host (macOS) | container | VM |
| --- | --- | --- | --- | --- |
| Workspace writes | Staged through OverlayFS (FUSE); nothing lands before apply | Staged through OverlayFS (macFUSE) | Staged through OverlayFS | Copy-on-write workspace served over virtio-fs |
| Writes to other host paths | `--filesystem sandbox`: namespaces, projected root, and Landlock | `--filesystem sandbox`: Seatbelt write scope | Image user space and configured mounts | Separate guest; root writes go to a temporary upper and are discarded on exit |
| Reads | Ambient by default; under `--filesystem sandbox`, constrained by the projected root and Landlock | Ambient by default; `--safe` requests a Seatbelt read scope, but current Run evidence still reports reads as ambient (see [known limitations](known-limitations.md)) | Image contents and configured mounts | Only rootfs and declared mounts; `--rootfs host` exposes host content |
| Sensitive paths in the view | `--access` rules and the `--safe` preset, enforced through OverlayFS | Same as Linux | Same as Linux | Same, applied to the root view |
| Network (selective policy) | Cooperative proxy, bypassable | Under `--safe`, blocks direct connections and enforces selective rules at the proxy | Cooperative proxy, needs host networking | Non-bypassable smoltcp data plane |
| Network (deny all) | Private network namespace | Seatbelt blocks non-loopback IPs and ambient Unix sockets | `--container-network none` | `--overlaynet off` |
| HOME and credentials | `--safe`: copy-on-write HOME view; credentials require an explicit `--pass-env` | `--safe`: temporary HOME; cannot read the original home directory | Only explicit mounts and passed variables | Rootfs-dependent; `--rootfs host` preserves the host HOME |
| Child processes | user/mount/PID namespaces; process-group cleanup | Process-group cleanup | Container process tree | Inside the guest kernel |
| Is `--safe` available? | Yes | Yes | **Refuses to start** (missing complete enforcement boundary) | Yes, requires `auto` networking |

## Single-node daemon boundary {#daemon}

The table describes native `pvisor run` executors, not `pvisor-daemon`. The daemon's current external rootless Podman backend shares the host kernel, installs private namespaces/no-new-privileges/`cap-drop=ALL` and resource limits, and requires prepared execd/egress services. It has no native VM, stage/apply, checkpoint/fork or pVisor network-policy integration. Unsupported network-policy requests are rejected; rootless slirp4netns is not deny-all egress.

Trust the host account, Podman configuration/hooks and prepared image. Other local users may reach native published loopback ports: daemon endpoint authentication alone does not protect those ports. Real upstream service authentication and host network controls are required for that risk. No hostile multi-user isolation or security-audit claim is made. See [daemon runtime boundaries](../guides/daemon/boundaries.md) for the image contract and current upstream egress capability conflict.

## Reading the table

- **A control in one dimension does not raise another.** A staged file does not prove the network is isolated, and a captured model request does not prove no other connection exists.
- **No executor currently claims complete subprocess enforcement.** As a result, `--strict` (which requires non-bypassable enforcement evidence for every requested dimension) exits with `UnsupportedPolicy` on host, container, and VM. Use it to verify fail-closed behavior; it is not a ready-made stronger sandbox preset.
- **The macOS VM is not a hostile multi-tenant boundary**: the VMM still holds the calling user's host permissions.
- **Descendants that actively leave the process group** are outside host process-group cleanup.

## Verify it in the Run Bundle

The Safety boundary section of `pvisor status --review` lists the controls and warnings actually installed for each dimension; in `--json`, `executor_observations` is the enforcement evidence, and the `safety.*` summary is derived from it. See [capabilities, evidence, and assurance boundaries](../concepts/capabilities-and-evidence.md) for what the levels (`Unenforced`, `Cooperative`, `Enforced`) mean.
