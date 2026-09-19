# Isolation mechanisms and limits

A copy-on-write workspace and a security boundary solve different problems. OverlayFS retains file changes for review. The executor and installed host controls determine whether the process can reach paths, sockets or resources outside that view.

## Current executors

| Executor | Mechanisms | Limits to keep explicit |
| --- | --- | --- |
| Linux host | Rootless launcher, user/mount/PID namespaces, projected root, negotiated Landlock, descriptor cleanup and capability dropping | Kernel and host configuration matter; selective proxy networking remains cooperative |
| macOS host | Generated Seatbelt profile with staged write controls; deny-all socket policy when requested | Reads and selective network access remain ambient/cooperative; staged mounts require macFUSE |
| Native OCI container | Linux OCI runtime, image userland and configured mounts/network | Does not claim complete enforcement of every capability dimension |
| libkrun VM | Separate Linux guest kernel, virtio-fs workspace and smoltcp network path | Requires KVM or HVF; host connectors and shared files still form part of the boundary |

Check the actual Run Bundle. Configuration expresses a request; installed controls and their evidence describe what happened. Safe-best-effort can report degraded controls. `--strict` fails admission when any required dimension lacks enforcement evidence; current executors do not claim complete Subprocess enforcement, so this is not a ready-made stronger sandbox preset.

## Workspace and lifecycle

Enable an explicit stage for a reviewable workspace:

```bash
pvisor run --stage ../stage-001 -- codex
```

Without an OverlayFS option, the host command may write the project directly. The stage does not roll back remote API calls or writes outside its covered workspace.

The host process executor creates a process group, sends termination signals to the group on completion or cancellation, and escalates after the grace period. It also bounds output draining when descendants hold pipes open. A process that leaves the group requires stronger platform containment; process-group cleanup alone is not a complete descendant boundary.

CLI checkpoints require stopped Runs and save the upper layer. They do not save process memory or freeze every lower-layer host file. Embedded AgentCtl participants can cooperate with quiescence, which does not turn arbitrary subprocesses into checkpointable processes.

## Network boundary

Host/container selective routing uses an explicit proxy and can be bypassed by clients that ignore it. Deny-all host policies and VM networking use different enforcement mechanisms. The VM data plane supports IPv4 TCP with DHCP and synthetic DNS; unsupported traffic fails closed. See [OverlayNet](overlaynet.md) for supported protocols and connector limitations.

## Future designs

LiteBox, Firecracker, Virtualization.framework execution and general transparent host/container interception are design directions, not selectable production backends. Fleet admission, attestation and tenant isolation require their own implementation and acceptance evidence. They must not be inferred from the current Run model.

Provider setup belongs in [Execution environments](../guides/execution.md). Interpretation of evidence belongs in [Security and evidence](../concepts/security-evidence.md).
