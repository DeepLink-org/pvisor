# Host executor

Host is the default: use the host kernel and installed toolchain. It fits local unattended execution with later file review.

```bash
pvisor run --executor host --stage ../stage-host -- /bin/sh
```

## Three independent switches

| Need | Option | Effect |
| --- | --- | --- |
| Reviewable copy-on-write workspace | `--stage PATH`, `--safe`, or `--ask` | Changes stay staged until apply |
| Path restrictions | `--filesystem sandbox` | Platform access controls |
| Block ordinary network egress | --overlaynet-deny-all | Platform network boundary |

Staging does not restrict reads and networking does not enable filesystem controls. Safe combines presets; see [`--safe` presets](../../reference/cli.md#safe-参数预设).

## Platform mechanisms

| Platform | Filesystem controls | Network boundary | Mounting |
| --- | --- | --- | --- |
| Linux | Rootless user/mount/PID namespaces, projected root, chroot, negotiated Landlock | Deny-all creates private network namespace | FUSE |
| macOS | Seatbelt read/write scope | Deny-all blocks non-loopback IP and ambient Unix sockets; retains declared local proxy/AgentCtl/private IPC | macFUSE |

`--safe` mode:

- **macOS:** Seatbelt enforces staged writes and limits connections to assigned loopback proxy ports. Temporary HOME prevents direct original-HOME reads. Current evidence still reports reads as ambient; see [limitations](../../security/known-limitations.md).
- **Linux:** requires rootless namespaces, synthetic root, chroot, Landlock, and private HOME staging. Selective egress uses a cooperative supervisor proxy; direct sockets can bypass it. Use VM or deny-all for mandatory networking.

## Gaps

- Default host filesystem visibility is unrestricted without sandboxing.
- Selective Linux host proxies can be bypassed by ignored proxy settings or direct sockets.
- Linux overlay deny does not hide secrets at original paths outside the view.
- Cleanup targets managed process groups and bounds output-pipe waiting; detached descendants are outside group cleanup.

## Verify

Use `pvisor status --review` for Safety boundary, or `--json` for fields such as `safety.network_non_bypassable`. See [evidence](../../concepts/capabilities-and-evidence.md).
