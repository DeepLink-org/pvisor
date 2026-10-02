# Isolation mechanisms and limits

A copy-on-write workspace and a security boundary solve different problems. OverlayFS retains changes for review; the executor and installed host controls determine access to paths, sockets and resources beyond that view.

## Current executors

| Executor | Mechanism | Limits to state explicitly |
| --- | --- | --- |
| Linux host | `--filesystem sandbox` enables a rootless launcher, user/mount/PID namespaces, projected root and negotiated Landlock; `--overlaynet-deny-all` independently enables a private network namespace | Depends on kernel and host configuration; selective proxy networking remains cooperative; default host filesystem view is unrestricted |
| macOS host | `--filesystem sandbox` enables Seatbelt filesystem controls; `--overlaynet-deny-all` independently enables deny-all socket policy; `--stage` independently selects a staged workspace | Without corresponding policies, reads and selective networking remain ambient/cooperative; staging mounts require macFUSE |
| Native OCI container | Linux OCI runtime, image userspace and configured mounts/network | Does not claim complete enforcement across all capability dimensions |
| libkrun VM | Separate Linux guest kernel, virtio-fs workspace and smoltcp network path | Requires KVM or HVF; host connectors and shared files remain part of the boundary |

Inspect the specific Run Bundle. Configuration expresses a request; installed controls and evidence show what happened. See [Capabilities and evidence](../concepts/capabilities-and-evidence.md). safe-best-effort may report downgraded controls. `--strict` rejects a Run when any required dimension lacks enforcement evidence. Current executors do not claim complete Subprocess enforcement, so strict is not a ready-to-use stronger sandbox preset.

## Workspace and lifecycle

Filesystem access, network isolation and staging are independent settings. Host defaults preserve the host filesystem view. Use `--filesystem sandbox` to restrict paths and explicitly enable staging to review changes:

```bash
pvisor run --stage ../stage-001 -- codex
pvisor run --filesystem sandbox --overlaynet-deny-all -- codex
```

See [Staging and storage](../reference/cli.md#暂存与存储) for default writes/cleanup and [Capabilities and evidence](../concepts/capabilities-and-evidence.md) for staging coverage. ZCode's Linux host adapter grants persistent write access to application state directories; see [CLI reference](../reference/cli.md).

The host process executor creates a process group, signals the group on completion/cancellation and escalates termination after a grace period. Reads also have a deadline when descendants retain output pipes. Processes that deliberately leave the group need stronger platform controls; group cleanup alone is not a complete descendant isolation boundary.

`fork` requires a stopped Run. See [Execution model](execution-model.md) for snapshot scope. Embedded AgentCtl participants can cooperate with quiescence, but that does not make arbitrary subprocesses resumable from a checkpoint.

## Network boundaries

See [Network boundaries](../guides/policies/network.md#网络边界) for controls, bypassability and VM protocol coverage, and [OverlayNet](overlaynet.md) for mechanisms.

See [Executors](../guides/executors/index.md) for environment configuration and [Capabilities and evidence](../concepts/capabilities-and-evidence.md) for interpreting evidence.
