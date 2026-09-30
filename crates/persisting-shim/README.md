# persisting-shim

`containerd-shim-pvisor-v2`: a [containerd Runtime v2] shim for pVisor,
registered under the runtime type `io.containerd.pvisor.v2` (the binary name
follows containerd's discovery rule: dots become dashes, last two
components, `containerd-shim` prefix).

This is **M2 scope**: the host process path with full task IO. The shim
implements the lifecycle every caller needs (`create`/`start`/`kill`/
`wait`/`delete`/`state`/`pids`/`connect`/`shutdown`), exec into running
tasks (`Exec` -> `Start(exec_id)` with `TaskExecAdded`/`TaskExecStarted`
events), shim-owned FIFO/PTY IO with `CloseIO` (stdin keepalive, so
`docker run -i` sees EOF) and `ResizePty`, task events, and a cgroup v2
subset (pids/memory/cpu). Stats, pause/resume, checkpointing, and the
pod-level Sandbox API are not implemented yet — the generated ttrpc trait
defaults report them as unsupported, which containerd tolerates.

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
  serializable `ContainerPlan` (cross-platform, unit-tested). Non-enforced
  spec features (seccomp, hooks, maskedPaths, time namespaces) surface as
  warnings in the shim log.
- `child.rs` is the self-exec pipeline (the house `INTERNAL_SANDBOX_ARG`
  pattern): `Create` produces a created-but-not-running init process that
  blocks on a start pipe; `Start` releases it. Exits are reaped by the
  framework's SIGCHLD monitor and mapped back onto tasks.
- `service.rs` implements the ttrpc Task service and publishes
  `TaskCreate/TaskStart/TaskExit/TaskDelete` events through the containerd
  event publisher.

## VM executor (M3, feature `vm`)

Bundles annotated with `"io.pvisor.executor": "vm"` run in a libkrun
microVM instead of host namespaces: one VM per task, rootfs shared
read-write over virtio-fs (`/dev/root`), stdio over the virtio-console, VM
shape via `io.pvisor.vm.cpus` / `io.pvisor.vm.memory-mib` (default 2 vCPU /
512 MiB). Kill signals the VM runner (destroying the VM); the guest exit
code propagates through the task exit status. The implicit vsock is
disabled, mirroring pVisor's VM executor posture.

Build with the feature (Linux host, or cross via `just shim-vm-build`):

```bash
cargo build -p persisting-shim --features vm
```

Host requirements: `/dev/kvm` and `libkrunfw` on the library path.
Not mapped into VMs yet (logged as warnings): spec bind mounts and cgroup
limits (the VM shape is the resource boundary).

## Guest agent and exec-in-VM (M5, feature `vm`)

VM tasks boot the shim binary itself as a guest agent: at boot the
(statically linked) binary is copied into the rootfs and the guest init
helper starts it (`io.pvisor.vm.agent=off` disables it). The agent listens
on vsock port 0x7076; libkrun proxies host connections from
`<bundle>/pvisor-agent.sock` into the guest (so `docker exec` /
`kubectl exec` work on VM tasks):

- one agent connection per exec; frames are `[channel][length][payload]`
  with JSON control messages (`exec_start`/`started`/`exited`)
- the guest process starts at `Exec` time (containerd's `Start` reports
  the pid; there is no two-phase gate across the VM boundary)
- killing an exec drops the connection; the agent SIGKILLs the process
- tty exec in VMs is not supported yet

Pod-level VM sandboxes (per-container rootfs and namespaces inside one VM
per pod, TC/TAP pod networking) remain the open item for the VM path.

## M4 status

Pod-level sandboxes are in (host path): Create/Start/Wait/Stop/Shutdown/
Platform/Ping/Status on the Sandbox service, a holder process that owns the
pod namespaces (pause replacement, including shareProcessNamespace pods),
and containers that join the shared namespaces unless their spec overrides
them. Pod-level **VM** sandboxes return a clear error until the guest agent
lands (M5); per-container VMs via `io.pvisor.executor=vm` keep working.

## M2 limitations (deliberate)

- Exec joins the init process's namespaces via `setns`; it needs `CAP_SYS_ADMIN`
  (rootful containerd). Rootless exec is not supported yet.
- Pod-level Sandbox API and per-pod VMs are M3/M4 (Kata-style); today each
  task runs in its own namespace set on the host.
- Seccomp profiles, OCI hooks, maskedPaths/readonlyPaths, device cgroups,
  and systemd cgroup delegation are ignored (logged as warnings).
- Stats, pause/resume, and checkpointing are unimplemented.
- VM exec: no tty, and the process starts at `Exec` time (see above).

## Developing

Pure logic (`plan`, `state`, `caps`, `cgroup`, mount option parsing) is
cross-platform and unit-tested on any host:

```bash
just test shim          # or: cargo nextest run -p persisting-shim
```

The syscall paths compile-check cross-platform; full builds need Linux:

```bash
just shim-check         # cargo check + clippy for x86_64-unknown-linux-gnu
```

## Linux acceptance smoke test

On a Linux host with containerd ≥ 1.7:

```bash
cargo build --release -p persisting-shim
sudo install -m755 target/release/containerd-shim-pvisor-v2 /usr/local/bin/
sudo systemctl restart containerd
sudo ctr run --runtime io.containerd.pvisor.v2 -t --rm \
  docker.io/library/busybox:latest pvisor-smoke echo hello from pvisor
```

Expected: the container prints `hello from pvisor`, the task exits with
status 0, and `journalctl -u containerd` shows the four task events.
With Docker ≥ 23: `docker run --rm --runtime=io.containerd.pvisor.v2
busybox echo hello` (registration optional, see above); interactive and
exec flows are the M2 additions to try: `docker run -it --rm
--runtime=io.containerd.pvisor.v2 busybox sh`, `docker exec <id> ls /`,
and `echo hi | docker run -i --rm --runtime=io.containerd.pvisor.v2 busybox cat`.

[containerd Runtime v2]: https://github.com/containerd/containerd/blob/main/docs/runtime-v2.md
