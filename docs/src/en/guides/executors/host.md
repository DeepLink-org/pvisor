# Host executor

Host is the default executor: the command uses the host kernel and installed toolchain directly. It fits best when you want to run an agent unattended on your own machine and review the changes afterward.

```bash
pvisor run --executor host --stage ../stage-host -- /bin/sh
```

## Three independent switches

| Need | Option | Effect |
| --- | --- | --- |
| A reviewable copy-on-write workspace | `--stage PATH`, or `--safe`/`--ask` | Workspace changes go to the stage and do not touch the project before apply |
| Path access restrictions | `--filesystem sandbox` | Enables the platform's filesystem access controls (below) |
| Deny all ordinary network egress | `--overlaynet-deny-all` | Installs the platform's network boundary (below) |

None of the three implies another: enabling staging does not restrict reads, and network options do not enable filesystem restrictions. `--safe` combines them into one preset; see the [`--safe` parameter preset](../../reference/cli.md#safe-参数预设).

## Platform mechanisms

| Platform | Filesystem controls | Network boundary | Stage mounting |
| --- | --- | --- | --- |
| Linux | rootless launcher: user/mount/PID namespaces, a projected root, `chroot`, and a kernel-negotiated Landlock | `--overlaynet-deny-all` creates a private network namespace | FUSE |
| macOS | Seatbelt read/write scope | `--overlaynet-deny-all` blocks non-loopback IPs and the host's ambient Unix sockets with Seatbelt, keeping loopback proxies, AgentCtl, and Run-private IPC | macFUSE |

What `--safe` combines on each platform:

- **macOS**: Seatbelt enforces staged writes and allows connections only to the loopback proxy ports pVisor assigns. The agent uses a temporary HOME and cannot read the original home directory directly. Read scope is judged by Run evidence: current evidence reports reads as ambient, see [Known limitations](../../security/known-limitations.md).
- **Linux**: requires rootless namespaces, a synthetic root, chroot, Landlock, and a copy-on-write HOME view. Selective egress is forwarded cooperatively through a supervisor loopback proxy, and a **direct socket can still bypass it**; use a VM or deny-all when you need a non-bypassable network boundary.

## Known gaps

- By default (without `--filesystem sandbox`) the host filesystem view is unrestricted and reads are ambient.
- Selective network policy (allow/deny lists) is cooperative on a Linux host: a client that ignores the proxy or opens a socket directly can bypass it.
- On Linux, overlay deny rules do not hide secret files at original paths outside the workspace view.
- A process is cleaned up by process group after completion or timeout, with a bounded wait on output pipes; a descendant that deliberately leaves its process group is not covered by process-group cleanup.

## Verify in the Run Bundle

After a run, use `pvisor status --review` to read the Safety boundary section, or `--json` to read fields such as `safety.network_non_bypassable`. See [Capabilities, evidence, and guarantee boundaries](../../concepts/capabilities-and-evidence.md) for what the evidence means.
