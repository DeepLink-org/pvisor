# pvisor-guest

Rust PID 1 supervisor and the platform-independent launch message in `/.pvisor-guest.json`. It initializes Linux virtual filesystems, applies process limits, supervises the workload and reports its exit to the VMM.

Guest initialization also provides two environment defaults that container
runtimes normally inject and container images routinely omit. When the image
ships no `/etc/hosts`, the supervisor writes the conventional loopback
entries (`127.0.0.1 localhost` and the IPv6 forms); without them `localhost`
falls through to the synthetic DNS resolver, which answers every name from
its egress range and turns loopback binds into `EADDRNOTAVAIL`. When the
launch environment omits `HOME`, the supervisor defaults it from the guest
`/etc/passwd` entry of the effective uid — an unset OCI user means root,
matching Docker's defaulting — for the workload and for sidecars (and the
command shells they spawn), falling back to `/` when no entry matches.

Before reading launch configuration or starting any child, PID 1 disables normal
dump access and claims a host-generated 128-bit exit token on the root virtio-fs
device. The kernel-supplied caller must be guest root/PID 1; ordinary workloads
and other shares cannot claim or report. Token bytes stay in PID 1 memory, never
in launch JSON, argv, environment or an inherited token descriptor. After reaping
and final synchronization, PID 1 reports that token and a checked `0..=255` code
once. Missing/failed handshake prevents workload start; there is no legacy ioctl
fallback. Shared request numbers and byte framing live in `exit_status`.

Whole-VM snapshots preserve the capability in both PID 1 RAM and host device
state, continuing the captured supervisor rather than rotating its token. Host
snapshots remain private authority. This relies on a trusted guest kernel and
PID 1; guest privileges capable of taking over either remain outside the claim.
The VMM rejects legacy reports, duplicates and invalid codes, and does not let a
guest success override abnormal vCPU termination.

Linux may reach PID 1 before the initial console can supply standard descriptors.
On a minimal image without `/dev`, Rust's standard-stream check would then abort
before `main` could mount devtmpfs or report an error. A PID-1-only libc constructor
runs before that check: it fills only closed standard descriptors from `/dev/null`,
creating the `/dev` mountpoint and mounting devtmpfs if needed. Existing descriptors
and their flags are preserved; replacement descriptors survive exec. Normal guest
initialization still waits for the named virtio ports and redirects workload I/O.
The constructor uses only allocation-free operations and does nothing outside PID 1.
Bootstrap failure retains the runtime's fail-closed behavior. The standard-descriptor
test covers all closed/open combinations; the CLI's ignored minimal-rootfs KVM
test also exercises the constructor and devtmpfs path through repeated cold boots.

Before reporting workload exit, PID 1 synchronizes every visible virtio-fs
mount and checks its result. This drains guest writes that outlive ordinary
file closes. A writeback failure is reported and changes the final status to
125, including when the workload itself exited successfully. Mount paths are
decoded from the bounded kernel mount table; a failure on one mount does not
skip attempts to drain the others. The host stage must be consumed after this
completion boundary, rather than after a guest close or a host-queue drain.

## Private temporary filesystem

`GuestConfig.temporary_filesystem` optionally requests a tmpfs before the workload starts. The contract is independent of CPU architecture:

```json
{"temporary_filesystem":{"path":"/.pvisor-tmp-unique-id","size_bytes":67108864}}
```

The path must be absolute, have an existing parent, and identify a fresh directory. Parent traversal, NUL, zero capacity and overlap with the workspace mount are rejected. Initialization creates the directory exclusively, then mounts tmpfs with `nodev,nosuid,relatime` and mode `1777`. Existing image contents or mounts are never silently covered; creation or mount failure aborts launch. Executable temporary files are supported.

Capacity is charged inside guest RAM; it does not allocate a second VM memory budget. A full filesystem reports normal write errors such as ENOSPC. tmpfs is ephemeral across cold launches. Its pages and kernel state are part of a whole-VM RAM snapshot; callers must preserve the existing RAM snapshot contract when restoring it. It is outside the host workspace review/apply stage.

The pVisor VM executor selects a unique directory and sets default TMPDIR to it. Capacity is one quarter of effective guest RAM, capped at 64 MiB. An explicit invocation TMPDIR disables this default, as does overlap with the workspace. The original image `/tmp` remains available. Other GuestConfig consumers that omit the optional field retain their existing behavior; JSON serialization omits it when absent.

The CLI snapshot launcher and container shim omit this option. Launch-message unit tests do not establish RAM snapshot/fork behavior with the executor-selected tmpfs.

## Sidecar processes

`GuestConfig.sidecars` optionally lists daemons started alongside the workload:

```json
{"sidecars":[{"argv":["/opt/payload","--serve"],"env":{"P":"1"},"log":"/.payload.log"}]}
```

Sidecars are advisory by contract. Each runs with the workload environment
overlaid by its own (`env` entries win on conflicts), stdin and stdout on
`/dev/null`, and stderr appended to the absolute guest `log` path. A spawn
failure or a sidecar exit is reported on the console and never affects the
workload, its exit code or the reported guest status; nothing restarts a
sidecar. argv[0] addresses a file the host executor staged into the rootfs;
the supervisor performs no lookup of its own. Validation (program present,
absolute log path, NUL-free words, valid environment names) runs with the
whole launch contract before any side effect. Consumers that omit the field
keep their existing behavior; JSON serialization omits it when absent.
`GuestConfig.bridges` optionally adds AF_VSOCK-to-guest-TCP relays (advisory:
a failed listener is reported and never affects the workload), and
`GuestConfig.shares` lists extra virtio-fs mounts (read-only shares mount
`ro,dax=never`; read-write plain passthrough; a failed mount is fatal).

## One workload supervisor

Jobs and controlled workloads enter the same `workload` function and use
`linux/process.rs` for command validation, resource limits, process creation,
signals, terminal allocation and reaping. There is no Docker-specific guest
entry point or supervisor. The host translates Docker image/CLI policy into
generic launch options, including its capability bounding set.

`GuestConfig.process` optionally supplies guest identity, capability bounds,
session creation, stdin ownership, PTY dimensions and working-directory creation.
Omission retains bootstrap identity/session and inherited console descriptors.
`GuestConfig.mounts` declares additional shares, optionally populating an empty
share from its image destination. Shares carry VMM tags, never host source paths.

One blocking PID-1 reaper handles the main workload, exec processes and adopted
orphans. It observes exits before consuming them, serializes process registration
and signalling with reaping, and routes each exec status to its own waiter.
Exec handlers never call waitpid themselves. Main exit stops new admissions,
terminates remaining guest processes and reaps them before the existing final
virtio-fs synchronization and exit report. Process groups created by the launcher
are cleaned up when their leader exits, including exec groups.

## Optional control adapter

`GuestConfig.control` enables a vsock listener on port 1025; omission starts no
listener. The adapter accepts only host-CID peers. Bounded
frames support ping, main stdin attachment, process-group signals, exec and
TCP tunnels to guest loopback services. Exec inherits launch environment and
working directory unless overridden, keeps stdout/stderr separate without a
PTY, and reports 126/127 for execution errors. TTY mode combines output on the
PTY and supports initial dimensions; tracked native execs support live resizing through the PID-fenced registry.

Both host and guest use `control`'s shared protocol definitions. Raw vsock
accept is required: `UnixListener::accept` rejects AF_VSOCK peer addresses.
Protocol tests reject oversized and unknown-field frames; real VM tests remain
necessary for terminal, mount, networking and process-lifecycle behavior.
TCP tunnel completion and errors shut down companion transport operations and
join the input pump, so cloned control descriptors cannot retain finished
tunnels. Explicit input EOF remains a TCP write-half close, preserving the full
response from the guest application.
Architecture guards keep spawning/signalling/reaping out of the transport
adapter. Linux supervisor tests cover limits, exit statuses, orphan reaping and
concurrent exec routing; native CLI acceptance exercises the same behavior in a VM.

### Native daemon operations

The optional control adapter also accepts tracked `Start` requests with explicit
`Started { pid }` acknowledgements, PID-fenced `Signal`/`Resize`, guest-local
`File` operations and `Info` observations. These use the same process launcher
and blocking PID-1 reaper as ordinary exec. Native transport binding variables
are removed from tracked exec environments. Only a matching live registered
child can be signalled or resized; exited/reused PIDs fail. Main exit still
closes admissions and cleans up all descendants.

File streams use bounded data frames and explicit EOF, preserve binary bytes and
run as guest root. Read/write reject special files; write may leave partial
content after a disconnect/error. Listing is limited to 1,024 immediate entries.
Metadata, permissions and symlink targets come from the guest filesystem.
File metadata carries guest mtime and inode change time (ctime); host protocol
adapters choose their wire field names, without substituting host timestamps.
`Info` reports launch binding and actual `/proc` memory/CPU counters, never API
credentials. Native failures include Linux errno for host protocol error mapping.
The guest does not implement HTTP, SSE or E2B Connect; those adapters live in the
daemon supervisor. Host protocol validation is separate from guest transport tests.

## Startup diagnostics

Host-selected `GuestConfig.env["PVISOR_STARTUP_PROFILE"] = "1"` enables optional
`pvisor-startup-profile` JSON on guest stderr. The native Job runner copies the host profiling switch into this launch setting,
including when ordinary environment projection omits host variables. Other
GuestConfig consumers may explicitly supply it. No launch-message field or guest protocol changes.
Private spans cover config read/validation, virtual filesystems and console/stdio
initialization, scratch/workspace/share mounts and networking, helper setup, and
workload process creation. A process-created marker does not prove application Ready.

The guest uses `clock=guest_monotonic_process_relative`, `epoch=guest-supervisor`
and guest PID; never align these timestamps with host/runner epochs or subtract
them from host Ready. Successful spans report `ok`; errors retain `unknown` and
best-effort logging cannot replace the original error. One entry timestamp is
captured before reading the config because the switch is not available earlier;
disabled subsequent spans capture no clock and emit nothing. Config parse failures
before the switch is known retain the existing error only. Config-read timing is frozen before initialization and emitted after guest stderr
is connected, so it excludes the subsequent console wait. Logging can perturb
startup. Collect profiled diagnostics separately
from uninstrumented performance samples. See the
[startup diagnostic tool](../../benchmark/startup/startup_profile.py).
