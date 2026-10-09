# pvisor-shim

`containerd-shim-pvisor-v2`: a [containerd Runtime v2] shim for pVisor,
registered under the runtime type `io.containerd.pvisor.v2` (the binary name
follows containerd's discovery rule: dots become dashes, last two
components, `containerd-shim` prefix).

The host process path provides full task IO. The shim
implements the lifecycle every caller needs (`create`/`start`/`kill`/
`wait`/`delete`/`state`/`pids`/`connect`/`shutdown`), exec into running
tasks (`Exec` -> `Start(exec_id)` with `TaskExecAdded`/`TaskExecStarted`
events), shim-owned FIFO/PTY IO with `CloseIO` (stdin keepalive, so
`docker run -i` sees EOF) and `ResizePty`, task events, and a cgroup v2
subset (pids/memory/cpu). The host pod-level Sandbox API is described below.
Stats, pause/resume and checkpointing are not implemented — the generated
ttrpc trait defaults report them as unsupported, which containerd tolerates.

## Registering the runtime

### containerd (Kubernetes path)

`/etc/containerd/config.toml`:

```toml
version = 2

[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.pvisor]
  runtime_type = "io.containerd.pvisor.v2"
  sandboxer = "shim"
```

with `containerd-shim-pvisor-v2` on the `PATH` of the containerd process.
`sandboxer = "shim"` activates the pod-level Sandbox API: one shim instance
per pod, the shim replaces the pause container with a namespace holder, and
the pod's containers share its uts/ipc/network namespaces (the CNI pod
netns is joined directly, so pod IP semantics hold). Kubernetes Pods select
the runtime through a RuntimeClass:

```yaml
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: pvisor
handler: pvisor
```

### Docker (23.0+)

Either register a short name in `/etc/docker/daemon.json`:

```json
{
  "runtimes": {
    "pvisor": { "runtimeType": "io.containerd.pvisor.v2" }
  }
}
```

or, with the shim on `PATH`, use the fully qualified name directly without
any registration: `docker run --runtime=io.containerd.pvisor.v2 ...`.

## How it works

```
containerd ──ttrpc── PvisorTask (Task service)
                         │ Create: plan from config.json + request mounts
                         │ re-exec self as init parent (A)
                         ├─ A: setns/unshare → mount rootfs+binds → fork G
                         │     → cgroup limits → relay readiness
                         └─ G: mount proc → open FIFOs/console PTY →
                               pivot_root → wait for Start → set ids/caps →
                               execve(args)
```

- `plan.rs` turns the OCI spec plus the `Create` request into one
  serializable `ContainerPlan` (cross-platform, unit-tested). Unsupported
  OCI constraints are rejected before task IO or child creation, rather than
  downgraded to warnings. Process checks also apply to Exec requests.
- `child.rs` is the self-exec pipeline (the house `INTERNAL_SANDBOX_ARG`
  pattern): `Create` produces a created-but-not-running init process that
  blocks on a start pipe; `Start` releases it. Exits are reaped by the
  framework's SIGCHLD monitor and mapped back onto tasks.
- `service.rs` implements the ttrpc Task service and publishes
  `TaskCreate/TaskStart/TaskExit/TaskDelete` events through the containerd
  event publisher.

## VM executor (feature `vm`)

Bundles annotated with `"io.pvisor.executor": "vm"` run in a libkrun
microVM instead of host namespaces: one VM per task, rootfs shared
read-write over virtio-fs (`/dev/root`), stdio over the virtio-console, VM
shape via `io.pvisor.vm.cpus` / `io.pvisor.vm.memory-mib` (default 2 vCPU /
512 MiB). Kill signals the VM runner (destroying the VM); the guest exit
code propagates through the task exit status. The shim configures a vsock
proxy for the guest agent (see below); this is not a network isolation claim.

Build with the feature (Linux host, or cross via `just shim-vm-build`):

```bash
just shim-vm-build
```

Host requirement: `/dev/kvm`. The static musl binary embeds its libkrunfw kernel.
Cross-builds need `PVISOR_KRUNFW_KERNEL_BUNDLE` pointing to extracted
`kernel.bin` and `kernel.json`; native Linux builds prepare firmware automatically.
OCI spec mounts, nonempty Linux configuration (including namespaces,
ID mappings and cgroups), `root.readonly=true`, and hostname configuration
are rejected for VM tasks: the runner does not install them. Snapshotter
request mounts used to materialize the rootfs are distinct from OCI spec
mounts and remain accepted. VM CPU/RAM annotations select VM shape; they
are not enforcement of OCI cgroup limits.

## Guest agent and exec-in-VM (feature `vm`)

VM tasks boot the shim binary itself as a guest agent: at boot the
(statically linked) binary is copied into the rootfs and the Rust
`pvisor-guest` supervisor starts it from `/.pvisor-guest.json`, without
a shell helper (`io.pvisor.vm.agent=off` disables it). The agent listens
on vsock port 0x7076; libkrun proxies host connections from
`<bundle>/pvisor-agent.sock` into the guest (so `docker exec` /
`kubectl exec` work on VM tasks):

- one agent connection per exec; frames are `[channel][length][payload]`
  with JSON control messages (`exec_start`/`started`/`exited`)
- the guest process starts at `Exec` time (containerd's `Start` reports
  the pid; there is no two-phase gate across the VM boundary)
- killing an exec drops the connection; the agent SIGKILLs the process
- tty exec in VMs is not supported yet


## Host pod-level sandboxes

The host path implements pod-level sandboxes: Create/Start/Wait/Stop/Shutdown/
Platform/Ping/Status on the Sandbox service, a holder process that owns the
pod namespaces (pause replacement, including shareProcessNamespace pods),
and containers that join the shared namespaces unless their spec overrides
them. Pod-level **VM** sandboxes are unsupported and return a clear error;
per-container VMs use `io.pvisor.executor=vm`.

## Limitations (deliberate)

- Exec joins the init process's namespaces via `setns`; it needs `CAP_SYS_ADMIN`
  (rootful containerd). Rootless exec is not supported yet.
- Pod-level sandboxes share host namespaces as described above; per-pod VMs
  are unsupported.
- Seccomp profiles, nonempty OCI hooks, maskedPaths/readonlyPaths, device
  nodes/device cgroups, sysctls, SELinux mount labels, rootfs propagation,
  time namespaces/offsets and other unimplemented typed Linux fields are
  rejected. Empty optional collections request no restriction and are accepted.
- Nonempty AppArmor/SELinux process labels, user names, OOM score adjustment,
  scheduler, I/O priority and CPU affinity settings are rejected for both
  init and exec processes.
- Host cgroup handling implements only `pids.limit`, `memory.limit`, and
  `cpu.quota`/`cpu.period` on cgroup v2. Other nonempty resource fields
  (including swap/reservation, CPU shares/cpuset, block IO, huge pages,
  network, RDMA and unified controls) are rejected. A usable cgroup path and
  write permissions are required; setup errors fail task creation.
  Systemd-style paths are translated to filesystem paths, **not** delegated
  through systemd.
- Stats, pause/resume, and checkpointing are unimplemented.
- VM exec: no tty, and the process starts at `Exec` time (see above).

## Security admission and remaining risks

The host execution tail applies numeric uid/gid/supplementary groups, umask,
rlimits, capability sets and `noNewPrivileges`, and fails on identity/limit
installation errors. Bounding capabilities are dropped before switching uid;
a failed required drop is fatal, not a warning. This does not establish complete
OCI conformance: non-root capability retention across uid changes is not
implemented, and privileged containerd/VM end-to-end enforcement has not been
validated by the unit suite. Host namespace joins and supported mount/root
read-only operations use syscalls; these are not evidence that every mount
option or nested mount restriction is correctly enforced.

VM init currently accepts only root uid/gid, no supplementary groups or umask,
no explicitly supplied capability sets (even empty ones), and no
`noNewPrivileges=true`. Init rlimits are passed to the guest supervisor.
VM exec additionally rejects nonempty rlimits, since the agent protocol carries
only argv/env/cwd. Neither the VM boundary nor VM shape substitutes for a
requested OCI process security policy.

Agent frame readers and writers limit each payload to **1 MiB**, including
control JSON, and reject larger lengths before payload allocation or reading.
Only EOF before a header is a clean close; truncated headers/payloads are errors.
This prevents the guest-selected near-4-GiB allocation, but does not bound
aggregate connections/threads, total output, JSON object overhead, or time spent
waiting for a peer. The vsock agent has no protocol authentication and can launch
root processes inside the guest. Those risks are not fixed by a frame cap.

Admission inspects fields retained by `oci-spec`'s typed deserializer, not raw
JSON schema validation. Unknown fields discarded by that library are not covered
by this rejection policy. Mount options, namespace/ID-map combinations and
inherited descriptors have not been comprehensively audited; in particular,
failure to enumerate inherited descriptors remains a warning. Common Docker/CRI
specs request unsupported security defaults and fail explicitly; remove a policy
only if that weaker boundary is intentionally acceptable, or use a runtime that
actually enforces it.

## Developing

Pure logic (`plan`, `state`, `caps`, `cgroup`, mount option parsing) is
cross-platform and unit-tested on any host:

```bash
just test shim          # or: cargo nextest run -p pvisor-shim
```

The syscall paths compile-check cross-platform; full builds need Linux:

```bash
just shim-check         # cargo check + clippy for x86_64-unknown-linux-musl
```

## Linux acceptance smoke test

On a Linux host with containerd ≥ 1.7, build the host-only shim:

```bash
cargo zigbuild --release -p pvisor-shim --target x86_64-unknown-linux-musl
sudo install -m755 target/x86_64-unknown-linux-musl/release/containerd-shim-pvisor-v2 /usr/local/bin/
sudo systemctl restart containerd
sudo ctr run --runtime io.containerd.pvisor.v2 -t --rm \
  docker.io/library/busybox:latest pvisor-smoke echo hello from pvisor
```

For VM support, replace the build command with
`just shim-vm-build release` and install
`target/release/containerd-shim-pvisor-v2`. This entry point prepares the
embedded kernel and builds the Rust guest automatically.

Expected: the container prints `hello from pvisor`, the task exits with
status 0, and `journalctl -u containerd` shows the four task events.
With Docker ≥ 23: `docker run --rm --runtime=io.containerd.pvisor.v2
busybox echo hello` (registration optional, see above); interactive and
exec flows to try: `docker run -it --rm
--runtime=io.containerd.pvisor.v2 busybox sh`, `docker exec <id> ls /`,
and `echo hi | docker run -i --rm --runtime=io.containerd.pvisor.v2 busybox cat`.

[containerd Runtime v2]: https://github.com/containerd/containerd/blob/main/docs/runtime-v2.md
