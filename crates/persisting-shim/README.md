# persisting-shim

`containerd-shim-pvisor-v2`: a [containerd Runtime v2] shim for pVisor,
registered under the runtime type `io.containerd.pvisor.v2` (the binary name
follows containerd's discovery rule: dots become dashes, last two
components, `containerd-shim` prefix).

This is **M1 scope**: the host process path. The shim implements the
lifecycle every caller needs (`create`/`start`/`kill`/`wait`/`delete`/
`state`/`pids`/`connect`/`shutdown`), bundle FIFO stdio, console sockets
(`docker run -t`), task events, and a cgroup v2 subset (pids/memory/cpu).
Exec, stats, pty resize, pause/resume, checkpointing, and the pod-level
Sandbox API are not implemented yet — the generated ttrpc trait defaults
report them as unsupported, which containerd tolerates.

## Registering the runtime

### containerd (Kubernetes path)

`/etc/containerd/config.toml`:

```toml
version = 2

[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.pvisor]
  runtime_type = "io.containerd.pvisor.v2"
```

with `containerd-shim-pvisor-v2` on the `PATH` of the containerd process.
Kubernetes Pods then select it through a RuntimeClass:

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

## M1 limitations (deliberate)

- No `exec`/`attach` into a running task (M2; needs the guest agent for VM
  workloads). `kubectl exec` / `docker exec` will fail against this runtime.
- Pod-level Sandbox API and per-pod VMs are M3/M4 (Kata-style); today each
  task runs in its own namespace set on the host.
- Seccomp profiles, OCI hooks, maskedPaths/readonlyPaths, device cgroups,
  and systemd cgroup delegation are ignored (logged as warnings).
- Rootful containerd is the tested path; rootless user namespaces work with
  explicit uid/gid mappings.

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
busybox echo hello` (registration optional, see above).

[containerd Runtime v2]: https://github.com/containerd/containerd/blob/main/docs/runtime-v2.md
