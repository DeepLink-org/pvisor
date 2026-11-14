# Native container profile

Owns the native Windows Docker-style container profile. Public callers use
`api` only. The existing Unix Docker profile keeps its richer cache, network and
staging implementation; this crate does not port Sessions or expose that state.

Image launch data and guest commands use `pvisor-guest`'s shared contracts and
supervisor. OCI files are extracted by PID 1 onto a private ext4 disk, never into
an NTFS approximation of Linux permissions or symlinks. Each instance has its own
WHP runner and same-user named-pipe owner. Separate command/signal console ports
let stop interrupt a running exec. Request IDs multiplex concurrent execs on the
command port; both ports require the matching embedded supervisor protocol.
The host console endpoints use overlapped Win32 I/O: a blocked receive must not
serialize a write through a duplicated synchronous pipe handle.
Admission is bounded to 32 requests per port. Each host output receiver holds at
most 64 frames; a stalled consumer fails its own delivery and later frames are
discarded by request ID instead of blocking unrelated callers or growing RAM.
Workload stdout/stderr never include the boot
console. Owner death closes a kill-on-close Windows Job containing the runner.
Owner creation requests breakaway when the caller's Job permits it. If Windows
denies breakaway and confirms Job membership, creation retries within that
enclosing Job, still with no inherited frontend handles. Restrictive host Jobs
(including CI runner Jobs) therefore bound the container's lifetime. The VM
runner still enters its own nested kill-on-close Job before any guest execution.

Supported: public linux/amd64 tar/gzip OCI images, foreground/detached runs,
names, auto-remove, literal environment overrides/removals, cwd/user/entrypoint
overrides, concurrent exec, piped stdin, real guest PTYs, list, retained/followed
logs, graceful stop with optional force-stop, start, remove and image untagging.
New launches default to bridge networking; explicit `none` attaches no NIC.
Ports, host mounts, build and Engine/Compose APIs are not provided.
The first profile uses a 4 GiB ext4 filesystem per instance. Image layer
decompression is capped at 2 GiB per layer. Failed/unconfirmed preparation is not
reusable. Start keeps the instance ID, pinned launch data, logs and ext4 writes;
it does not reapply image layers or pull a removed tag. Each start rotates the
owner generation and pipe names, so stale clients cannot address a new runner.
Start on a live Running instance is a no-op. Unknown state requires recreation.
Force-stop can leave an unclean filesystem; ext4 recovery is not a snapshot.

The built-in bridge allocates stable addresses in `172.17.0.0/24` under the
catalog lease; retained instances reserve their address until removal. The
gateway is `.1`. OverlayNet's existing userspace stack provides synthetic DNS
and IPv4 TCP egress; same-store peers exchange guest Ethernet packets without
a TAP, host bridge or privileged service. Windows peer transport uses loopback
UDP with leased, generation-fenced membership in a caller/SYSTEM-only ACL
directory excluded from guest shares. General UDP/ICMP egress, IPv6, host
networking, published ports and custom networks remain unsupported. Legacy
records without a network field remain isolated rather than silently joining.

Run and exec reuse the shared guest stdin/PTY launcher and PID-1 reaper.
Interactive data is limited to one outstanding 16 KiB chunk per request, with
explicit credits, EOF and cancellation; a blocked input does not block stop or
another exec. PTYs combine output on stdout and accept initial nonzero rows and
columns. Callers own console policy and provide a cancellable `InputSource`;
completion joins the scoped input worker. Unexpected exec attachment loss
cancels that exec's process group; explicit EOF permits continued execution.
The original main stdin remains independent of completion/log attachments.
Bootstrap readiness is an id-zero receipt after workload creation, not a ping
that a fast-exiting workload might never answer. Failed boot remains inspectable.

Stop selects the pinned image's OCI `StopSignal` (TERM by default), or the
caller's explicit Linux signal. Signal parsing is shared with Unix. A finite
grace escalates through the generation-bound owner; `Duration::MAX` never
escalates. Invalid signal/grace values fail before delivery.

Followed logs retain ordinary open files, not an extra RPC stream or daemon.
They drain both streams after owner teardown/auto-removal and end on generation
replacement. Writer errors propagate. Workload logs exclude exec/boot output.
Lease observation never recreates a missing lock file.

Image removal resolves exact tags, configuration digests or unique prefixes of
at least 12 hex digits. Retained instances pin image IDs even after tags change.
Force removes only tags, preserving container records and blobs. It is not disk
garbage collection. A multi-tag I/O failure may be partial; inspect before retrying.

`PVISOR_DOCKER_HOME` selects private container storage; default is the current
user's local application data. Do not place it in a directory writable by other
users. Records/locks fence launch identities and updates. Missing owner leases
without a terminal receipt mean unknown, never successful completion.

Build `tools/build_template.py OUTPUT` on Linux to produce the sparse,
gzip-compressed empty ext4 template. Windows installations place the resulting
`pvisor-rootfs.bin.gz` and the matching `libkrunfw.dll` beside `pvisor.exe`.
Users need WHP and supported hardware, not QEMU, WSL or Docker.

Run contracts with `mise run test -- pvisor-containers`. Windows end-to-end
acceptance must additionally pull a real image and exercise every command on WHP.
