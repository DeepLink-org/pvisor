# pvisor-guest

Rust PID 1 supervisor and the platform-independent launch message in `/.pvisor-guest.json`. It initializes Linux virtual filesystems, applies process limits, supervises the workload and reports its exit to the VMM.

## Private temporary filesystem

`GuestConfig.temporary_filesystem` optionally requests a tmpfs before the workload starts. The contract is independent of CPU architecture:

```json
{"temporary_filesystem":{"path":"/.pvisor-tmp-unique-id","size_bytes":67108864}}
```

The path must be absolute, have an existing parent, and identify a fresh directory. Parent traversal, NUL, zero capacity and overlap with the workspace mount are rejected. Initialization creates the directory exclusively, then mounts tmpfs with `nodev,nosuid,relatime` and mode `1777`. Existing image contents or mounts are never silently covered; creation or mount failure aborts launch. Executable temporary files are supported.

Capacity is charged inside guest RAM; it does not allocate a second VM memory budget. A full filesystem reports normal write errors such as ENOSPC. tmpfs is ephemeral across cold launches. Its pages and kernel state are part of a whole-VM RAM snapshot; callers must preserve the existing RAM snapshot contract when restoring it. It is outside the host workspace review/apply stage.

The pVisor VM executor selects a unique directory and sets default TMPDIR to it. Capacity is one quarter of effective guest RAM, capped at 64 MiB. An explicit invocation TMPDIR disables this default, as does overlap with the workspace. The original image `/tmp` remains available. Other GuestConfig consumers that omit the optional field retain their existing behavior; JSON serialization omits it when absent.

The CLI snapshot launcher and container shim omit this option. Launch-message unit tests do not establish RAM snapshot/fork behavior with the executor-selected tmpfs.
