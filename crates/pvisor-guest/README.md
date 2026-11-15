# pvisor-guest

Rust PID 1 supervisor and the platform-independent launch message in `/.pvisor-guest.json`. It initializes Linux virtual filesystems, applies process limits, supervises the workload and reports its exit to the VMM.

Launch-message paths always use Linux guest syntax, including when a Windows
host validates them. A leading `/` is absolute; drive letters and backslashes
do not supply a guest root or separators. Mount/tmpfs traversal and workspace
overlap checks use the same guest components on every host. Native execution
of the resulting command still belongs to the Linux supervisor.

Guest initialization also provides two environment defaults that container
runtimes normally inject and container images routinely omit. When the image
ships no `/etc/hosts`, the supervisor writes the conventional loopback
entries (`127.0.0.1 localhost` and the IPv6 forms); without them `localhost`
falls through to the synthetic DNS resolver, which answers every name from
its egress range and turns loopback binds into `EADDRNOTAVAIL`. When the
launch environment omits `HOME`, the supervisor normally defaults it from the guest
`/etc/passwd` entry of the effective uid — an unset OCI user means root,
matching Docker's defaulting — for the workload and for sidecars (and the
command shells they spawn), falling back to `/` when no entry matches.
An explicit process contract with `process.user = None` instead preserves an
absent `HOME`: the host has already selected its defaults and applied environment
removals. Explicit identity selection and launches that omit the process contract
retain the passwd default; container launches explicitly select their identity.
PID 1 also replaces Linux's unset `(none)` hostname with a DNS-safe
`pvisor-<12 hex digits>` name derived from the guest boot ID, preserving any
existing hostname. The fallback is distinct across cold boots and is never the
reserved `localhost` name (which breaks MariaDB account bootstrap). A generated
hosts file maps the hostname to loopback in addition to the conventional entries,
avoiding libc resolver retries during common HTTP server startup. An exact
supervisor-generated hosts template is refreshed on cold reboot; image-provided
hosts files and user edits remain unchanged. Ordinary host processes cannot call this initializer
to change the host UTS namespace.

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
skip tries to drain the others. The host stage must be consumed after this
completion boundary, rather than after a guest close or a host-queue drain.

## Private temporary filesystem

`GuestConfig.temporary_filesystem` optionally requests a tmpfs before the workload starts. The contract is independent of CPU architecture:

```json
{"temporary_filesystem":{"path":"/.pvisor-tmp-unique-id","size_bytes":67108864}}
```

The path must be absolute, have an existing parent, and identify a fresh directory. Parent traversal, NUL, zero capacity and overlap with the workspace mount are rejected. Initialization creates the directory exclusively, then mounts tmpfs with `nodev,nosuid,relatime` and mode `1777`. Existing image contents or mounts are never silently covered; creation or mount failure aborts launch. Executable temporary files are supported.

Capacity is charged inside guest RAM; it does not allocate a second VM memory budget. A full filesystem reports normal write errors such as ENOSPC. tmpfs is ephemeral across cold launches. Its pages and kernel state are part of a whole-VM RAM snapshot; callers must preserve the existing RAM snapshot contract when restoring it. It is outside the host workspace review/apply stage.

The pVisor VM executor selects a unique directory and sets default TMPDIR to it. Capacity is one quarter of effective guest RAM, capped at 64 MiB. An explicit invocation TMPDIR or a `pvisor.vm.env_unset` TMPDIR tombstone disables this default, as does overlap with the workspace. The original image `/tmp` remains available. Other GuestConfig consumers that omit the optional field retain their existing behavior; JSON serialization omits it when absent.

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
`GuestConfig.shares` lists extra virtio-fs mounts (read-only shares are
host-enforced read-only passthrough — the host may keep updating the
directory and guests observe those updates, unlike the physically immutable
`payloads_dir` share; read-write shares are plain passthrough; a failed
mount is fatal).

## One workload supervisor

Sessions and controlled workloads enter the same `workload` function and use
`linux/process.rs` for command validation, resource limits, process creation,
signals, terminal allocation and reaping. There is no Docker-specific guest
entry point or supervisor. The host translates Docker image/CLI policy into
generic launch options, including its capability bounding set.

`GuestConfig.process` optionally supplies guest identity, capability bounds,
session creation, stdin ownership, PTY dimensions and working-directory creation.
Omission retains bootstrap identity/session and inherited console descriptors.
`process.attached` defaults to false. When true, stdout/stderr are separate pipes
unless `tty` requests a real PTY, in which case output is combined on stdout with
the requested initial dimensions. `stdin = pipe` is interactive; `closed` gives
EOF via `/dev/null`, including with a PTY (only output remains terminal-backed).
Without a control adapter the supervisor pumps bootstrap stdin into interactive
input, routes pipe output by descriptor, and cancels/joins the input worker after
reaping, including on supervision failure. The writer is nonblocking so neither
idle host input nor a full child input buffer retains a completed workload. Pump
errors fail completion instead of silently publishing success. Control-enabled
launches keep their existing attachment ownership.

The VM executor privately deserializes `pvisor.vm.stage_stdio` as
`{"interactive":bool,"tty":[rows,cols]|null}` and translates it to these options;
zero dimensions fail launch validation. No metadata means ordinary Session behavior.
The host must hand foreground ownership through both CLI → owner and owner → VM
runner when stage output inherits a controlling terminal, even with closed stdin:
the VMM configures that terminal via `tcsetattr`, which can otherwise stop its
background process group with SIGTTOU. This handoff does not reopen guest stdin.
It builds env in order: inherit, scrub host cache credentials, remove inherited
loader paths, apply defaults, overlay explicit env, then remove the names in
`pvisor.vm.env_unset`. Removed PATH/HOME/TMPDIR are not recreated by stage launch.
`GuestConfig.mounts` declares additional shares, optionally populating an empty
share from its image destination. Shares carry VMM tags, never host source paths.
`source_file: true` instead binds the share's fixed ordinary `file` entry onto
a regular guest target, creating a missing target but refusing symlink/directory
targets. It cannot initialize a managed volume. The host must expose only the
admitted inode in this share; mounting an entire host parent is not this contract.
Cold launches may reuse the empty nonsymlink directory left beneath an internal
file-share mountpoint in the container's writable upper. Existing files, symlinks
and nonempty directories are refused rather than followed or covered.
The underlying share and final file bind are read-only when requested; host-side
write denial remains required because guest root can remount filesystems.

One blocking PID-1 reaper handles the main workload, exec processes and adopted
orphans. It observes exits before consuming them, serializes process registration
and signalling with reaping, and routes each exec status to its own waiter.
Exec handlers never call waitpid themselves. Main exit stops new admissions,
terminates remaining guest processes and reaps them before the existing final
virtio-fs synchronization and exit report. Process groups created by the launcher
are cleaned up when their leader exits, including exec groups.

## Optional control adapter

The Windows container profile uses two host-only virtio-console ports rather
than vsock. `SerialRequest`/`SerialReply` wrap the ordinary supervisor protocol in
monotonically increasing, nonzero request IDs. Responses may interleave; each
request still uses the same process launcher and PID-1 reaper. At most 32
operations execute on a port at once. Input frames carry the owning request ID;
one 16 KiB chunk per request is allowed until its credit is returned. EOF and
cancellation are separate, so a frontend disconnect cancels its exec while
intentional EOF permits continued execution. Main input and execs use the same
existing stdin/PTY adapter; the independent signal port admits stop, tracked
signals and resize. An id-zero Ready receipt confirms workload creation before
PID 1 reaps it, including immediate exits. Invalid or
reused IDs close the port, and unsupported/busy requests return explicit errors.
Host and embedded supervisor must be upgraded together.

`GuestConfig.root_disk` builds an OCI filesystem on a private ext4 block device.
Digest verification and extraction happen inside Linux, before mounting its
pseudo-filesystems; NTFS is not used to approximate image permissions or links.
A retained, successfully prepared container may relaunch with an empty layer
list to reuse the disk without reapplying the image. The host owns preparation
receipts and generation fencing; guest root is never authority to select a host
disk or claim a different container. When static networking is configured, disk
entry installs `/etc/resolv.conf` for its virtual gateway after extraction and
on retained starts. Image resolver symlinks are replaced without following them;
directory destinations fail launch. Offline disk entry leaves image DNS files
unchanged. Other rootfs profiles keep their caller-provided resolver policy.
Serial devices are opened before launch,
masked from workload paths and protected by the launch capability bounding set.

`GuestConfig.control` enables a vsock listener on port 1025; omission starts no
listener. The adapter accepts only host-CID peers. Bounded
frames support ping, main stdin attachment, process-group signals, exec and
TCP tunnels to guest loopback services. Exec inherits launch environment and
working directory unless overridden, keeps stdout/stderr separate without a
PTY, and reports 126/127 for execution errors. TTY mode combines output on the
PTY and supports initial dimensions; tracked native execs support live resizing through the PID-fenced registry.

Both host and guest use `control`'s shared protocol definitions. Raw vsock
accept is required: `UnixListener::accept` rejects AF_VSOCK peer addresses.
`control::parse_signal` is the common host-independent parser for OCI stop
signals and explicit container stop overrides on Unix and Windows. Signal
numbers always refer to Linux guest signals, never host Win32 process signals.
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

The host-only adapter exposes the opt-in experimental `ReclaimAllocator`
operation for a live registered PID/generation. Set
`GuestConfig.process.allocator_reclaim` to an absolute guest path to the matching
`libpvisor-reclaim.so` before launch. The host stages that library in the image or
an existing payload/share; the ordinary process launcher appends it to LD_PRELOAD
without replacing existing preloads. No option means no preload or channel.
This option is inherited by control-adapter exec/Start launches, each of which
gets its own socketpair and registered process identity.

PID 1 owns one endpoint of a private SOCK_SEQPACKET socketpair; the preloaded SO
owns a persistent detached worker on the other. `trim=false` checks the allocator;
`trim=true` calls glibc malloc_trim(0) in that worker while application threads
continue normally. No ptrace, shellcode, registers, syscall replay, signal handler
or application safe point is used. The worker blocks on the socket between
requests and has a 256 KiB stack reservation. The SO must match the target ABI;
the reference build uses the glibc 2.17 ABI, with no 2.34 layout requirement.
Static and secure-execution binaries can ignore preload; incompatible libraries
can prevent workload startup. Only opt in supported dynamic glibc workloads.

The channel descriptor intentionally survives same-PID exec. The constructor
recreates the worker when preload, the private binding environment and descriptor
survive. An atfork child handler only validates/closes the inherited socket;
it never starts a thread or calls the allocator. Fork descendants have no
independent control. A fork/vfork child that execs rejects the parent's binding;
an inode check prevents closing an unrelated descriptor that reused the old FD.
Clearing the environment, closing descriptors, or switching to an unsupported
executable loses control. An exec during an admitted request may lose its reply.

PID 1 validates packet size, magic, request sequence and kernel SCM_CREDENTIALS.
The process registry fences PID reuse/reaping during a request. Each response wait
is bounded to two seconds. An allocator/protocol/timeout error closes and retires
that registered channel, so stale replies cannot satisfy a later request. The
workload continues: there is no stop/kill or cancellation of an in-flight trim.
A timed-out call may still complete. The channel is cooperative instrumentation,
not a security boundary against the target manipulating its own descriptors or
libraries. Main exit retains normal descendant teardown and shared reaping.
See the [SO build, activation and native tests](../../tools/allocator-reclaim/README.md).

The optional control adapter also accepts tracked `Start` requests with explicit
`Started { pid }` acknowledgements, PID-fenced `Signal`/`Resize`, guest-local
`File` operations and `Info` observations. These use the same process launcher
and blocking PID-1 reaper as ordinary exec. Native transport binding variables
are removed from tracked exec environments. A tracked launch carries its own
rlimits (Linux resource names with soft/hard pairs, replacing the launch
limits; empty inherits them, the OCI exec default) and may mark its
environment as complete (`replace_env`) instead of an overlay onto the
launch environment — callers that already hold a full environment (OCI exec
specs) use this; HOME keeps its Docker-equivalent defaulting either way.
Only a matching live registered child can be signalled or resized;
exited/reused PIDs fail. Main exit still closes admissions and cleans up
all descendants. An output pump that fails before EOF is reported as a
`Failure` frame ahead of the exit code, so lost output never reads as a
clean exit on the host.

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
`pvisor-startup-profile` JSON on guest stderr. The native Session runner copies the host profiling switch into this launch setting,
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
