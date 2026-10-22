# pvisor-shim

`containerd-shim-pvisor-v2`: a [containerd Runtime v2] shim for pVisor,
registered under the runtime type `io.containerd.pvisor.v2` (the binary name
follows containerd's discovery rule: dots become dashes, last two
components, `containerd-shim` prefix).

The container-tier path (the default) provides full task IO. The shim
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

## Executor tiers

The `io.pvisor.executor` annotation names the isolation tier a task gets.
The vocabulary names what the caller receives, not which code path runs:

| Annotation value | Tier | Implementation |
| --- | --- | --- |
| *(absent, default)* / `container` | **container**: the task runs on the host kernel inside the OCI namespaces and cgroups (container-grade isolation) | self-exec pipeline (no runc) |
| `vm` | **microVM**: one libkrun microVM per task on its own guest kernel | VM runner (feature `vm`) |

Any other value is rejected at task creation (fail-closed): a misspelled
tier never silently downgrades the task.

There is deliberately no tier weaker than `container`: the shim is a
containerd runtime, its input is always an OCI task (image rootfs, at
least a private mount namespace, a cgroup), so a "bare host process"
cannot be expressed. Standalone pVisor's three executors map as follows:
`host` (host process) has no shim equivalent and its annotation value is
rejected here, `container` and `vm` map to the tiers above.

## VM executor (feature `vm`)

Bundles annotated with `"io.pvisor.executor": "vm"` (see
[Executor tiers](#executor-tiers)) run in a libkrun
microVM instead of host namespaces: one VM per task, rootfs shared
read-write over virtio-fs (`/dev/root`), stdio over the virtio-console, VM
shape via `io.pvisor.vm.cpus` / `io.pvisor.vm.memory-mib` (default 2 vCPU /
512 MiB). Kill signals the VM runner (destroying the VM); the guest exit
code propagates through the task exit status. The shim configures a vsock
proxy for the guest agent (see below); this is not a network isolation claim.

Build with the feature through the shared native build pipeline:

```bash
just build debug shim-vm
```

Host requirement: `/dev/kvm`. The static musl binary embeds its libkrunfw kernel.
Cross-builds need `PVISOR_KRUNFW_KERNEL_BUNDLE` pointing to extracted
`kernel.bin` and `kernel.json`; native Linux builds prepare firmware automatically.
Standard containerd/CRI Linux fields (namespaces, resources, cgroupsPath,
masked/readonly paths, sysctls, seccomp, SELinux mount labels, system mounts
like proc/sysfs/tmpfs) and host-LSM process fields (`apparmorProfile`,
`selinuxLabel`, `oomScoreAdj`) are accepted as no-ops for VM tasks — the
guest kernel cannot honor host constraints — while `root.readonly=true` and
hostname configuration are rejected (the runner does not install them).
Bind mounts are accepted as no-ops: kubelet unconditionally bind-mounts
`/etc/hosts`, `/etc/hostname`, `/etc/resolv.conf` and `/dev/termination-log`
into every pod, and the guest keeps its rootfs's own files for those paths
(host paths, ConfigMaps and emptyDir volumes do not appear in the guest). Snapshotter
request mounts used to materialize the rootfs are distinct from OCI spec
mounts and remain accepted. VM CPU/RAM annotations select VM shape; they
are not enforcement of OCI cgroup limits. The `io.pvisor.vm.log` annotation
(optional log filter, or `1` for `debug`) makes the VM runner write VMM and
early console diagnostics to `<bundle>/pvisor-vm.log` instead of stderr.

VM egress networking (feature `vm`): `io.pvisor.vm.network` selects the
task's network posture — `off`/unset (default: no network device at all),
`public`, or `allowlist` with `io.pvisor.vm.network.allow`
(`host[:port]`, comma separated) and optionally
`io.pvisor.vm.network.allow-private: "true"` for entries that resolve to
private space (intranet mirrors). Egress rides pvisor-overlaynet's virtio-net
smoltcp backend: the device is the guest's only network path, DNS is served
synthetically from the in-VM router (`/etc/resolv.conf` points at
192.0.2.1), every TCP flow terminates on the host and is re-originated only
when policy authorizes it, and per-task counters are logged at task delete
(`vm network snapshot ...`).

VM RAM dedup (feature `vm`): `io.pvisor.vm.ram-dedup: "true"` (default off,
only the literal `true` opts in) registers the task's guest RAM as KSM
mergeable via pvisor-vm's `advise_ram_dedup`. The advice is registration
only — it never merges anything by itself, never touches global sysfs, and
merging additionally requires the host KSM scanner
(`/sys/kernel/mm/ksm/run`), which the shim never configures; the operator
enables and tunes that. Enabling authorizes cross-workload page sharing and
its side-channel/trust-domain implications: use it for homogeneous fleets
inside one trust domain, not for mutually untrusted tenants. Advice
failures are logged (to the runner diagnostics, i.e.
`io.pvisor.vm.log`) and never fatal to the task.

VM task lifecycle: the runner is spawned detached at `Create` (it hosts the
VMM for the whole task), waits on a start pipe for `Start`, and exits with
the guest workload's exit code. The runner holds no end of the start pipe,
so a shim exit delivers EOF and the runner terminates instead of leaking.
Non-terminal task IO rides the named virtio-console ports
(`krun-stdin/stdout/stderr`); the guest supervisor waits for them, so
`CloseIO` EOF and piped stdin reach the workload with runc-compatible
semantics.

## Guest agent and exec-in-VM (feature `vm`)

VM tasks boot the shim binary itself as a guest agent: at boot the
(statically linked) binary is copied into the rootfs and the Rust
`pvisor-guest` supervisor starts it from `/.pvisor-guest.json`, without
a shell helper (`io.pvisor.vm.agent=off` disables it). The agent listens
on vsock port 0x7076; libkrun proxies host connections from
`/run/pvisor-agent-<task-id-hash>.sock` into the guest (so `docker exec` /
`kubectl exec` work on VM tasks; the socket cannot live in the bundle
because Unix socket paths are capped at 108 bytes and containerd bundle
paths exceed that; without `/run`, e.g. rootless, it falls back to the temp
directory):

- one agent connection per exec; frames are `[channel][length][payload]`
  with JSON control messages (`exec_start`/`started`/`exited`)
- the guest process starts at `Exec` time (containerd's `Start` reports
  the pid; there is no two-phase gate across the VM boundary)
- killing an exec drops the connection; the agent SIGKILLs the process
- exec rlimits travel in the `exec_start` control message and are applied
  in the guest before the process runs (same `prlimit64` contract as init)
- agent boot/session failures are recorded in `/.pvisor-agent.log` inside
  the rootfs (visible on the host through virtio-fs); spawn errors are
  returned to the caller as `Error` control messages
- tty exec in VMs is not supported yet


## Payload injection

`io.pvisor.inject` selects named payloads from a node-side catalogue to run
alongside the workload — e.g. a data-plane daemon (OpenSandbox `execd`)
without touching the image. The catalogue lives at
`/etc/pvisor-shim/injection.json` (`PVISOR_SHIM_INJECTION_CONFIG`
overrides) and is pure operator data, distributed like any node asset:

```json
{"payloads": {
  "opensandbox-execd": {
    "source": "/opt/pvisor/payloads/execd",
    "target": "/opt/opensandbox/execd",
    "argv": ["/opt/opensandbox/execd"],
    "env": {"EXECD_PORT": "44772"}
  }
}}
```

`source` is an absolute host file, `target` the absolute path it occupies
inside the container, `argv` defaults to `[target]`, and `env` overlays the
workload environment. A bundle annotation selects payloads by name (comma
separated): `"io.pvisor.inject": "opensandbox-execd"`. The shim knows
nothing about payload internals — no ports, protocols or versions — so
adding, updating or removing a payload is a catalogue change, never a shim
change. No annotation: no injection, no code-path difference.

Semantics by design:

- **Native PID 1.** The user command stays PID 1 with native signal and
  exit-code semantics; injected processes are advisory siblings spawned
  through the same namespace/pivot dance as the init child. They run with
  the workload identity (uid/gid, capabilities, rlimits) and the workload
  environment overlaid by their own.
- **Fail-closed admission.** An unknown payload name, duplicate name or
  target, a target already occupied in the image (file, directory or
  symlink), a target claimed by a spec/request mount, or a missing or
  non-regular source rejects the task at Create — an explicitly requested
  payload is never silently skipped.
- **Advisory lifecycle.** A sibling's death is logged and never fatal: the
  task exit status is always the init child's, Start never waits for
  injected processes, and nothing restarts them. Siblings live in the
  task's PID namespace and cgroup, so namespace teardown and
  `cgroup.kill` reap them with the workload.
- **Tier parity.** The container tier bind-mounts each payload read-only
  (`ro,nosuid,nodev` — deliberately not `noexec`) and forks one sibling per
  payload. The VM tier stages the binary into the rootfs (same
  materialization as the guest agent) and the guest supervisor starts it
  from the launch config (`pvisor-guest` `GuestConfig.sidecars`). Payloads
  marked `"share": true` (with the catalogue's `share_dir`) never copy in
  the VM tier: the directory is attached as one read-only DAX virtio-fs
  share (`ro,dax=inode` in the guest) and staged targets become symlinks
  into it — one host copy serves every sandbox on the node, and guest
  reads map it directly instead of duplicating page cache.
- **Logging.** Sibling stderr appends to
  `/var/log/pvisor-shim/<task-id>/inject-<name>.log`
  (`PVISOR_SHIM_INJECTION_LOG_DIR` overrides; a file that cannot open
  degrades to `/dev/null` with a shim warning). Files are per task and
  unbounded — collection and rotation belong to the node agent that
  distributes the catalogue. In the VM tier the log is
  `/.pvisor-inject-<name>.log` inside the guest root, host-visible through
  the virtio-fs tree. The workload's task IO (FIFOs/console) is never
  shared with an injected process, so `kubectl logs` stays byte-clean
  workload output.
- **Port bridges and exposure.** A payload `"bridge": <port>` (VM tier
  only) relays the guest service `127.0.0.1:<port>` to a listener on the
  same port inside the pod's network namespace — the chain is pod-netns
  proxy → vsock tunnel → guest relay. The bridge adds no authentication
  or encryption of its own: everyone who can reach `podIP:<port>` reaches
  the guest service, exactly the exposure a container-tier sibling has.
  Protect the port with the bridged service's own authentication and
  Kubernetes NetworkPolicy; the bridge only moves bytes. Stage-only
  payloads (`"run": false`) exist because VM guests cannot see volumes:
  an entrypoint wrapper the task command already names is staged, not
  spawned.

Injection applies to the init task only; exec processes and pod sandbox
holders never inject.

## Upstream OpenSandbox contract exceptions

Two OpenSandbox-generated pod containers are special-cased by upstream
contract markers in the OCI spec. Both are deliberate couplings: a missed
or renamed marker degrades to the normal path (never breaks a task), and
both are recorded here so an upstream rename surfaces as a behavior
regression, not a mystery.

- **Egress sidecar** (env `OPENSANDBOX_EGRESS_RULES`): always runs on the
  container tier even under a pod-wide `executor=vm` annotation — as a
  microVM it would police an empty namespace of its own and the pod would
  never become ready. Its Create publishes the pod's policy at a
  sandbox-scoped gate file; a VM workload that sees the egress-auth pod
  annotation boots its overlaynet attachment from that policy instead of
  the `io.pvisor.vm.network` annotation (deny-by-default rules become an
  allowlist, allow-by-default keeps the network public with deny rules).
  The 3-second gate window only spans the sidecar's Create, which kubelet
  issues milliseconds after the workload's Start.
- **execd-installer init container** (CRI annotations: container name
  `execd-installer` plus an `/execd` image): under `io.pvisor.executor=vm`
  pods it becomes a **synthetic success** — the task reports readiness,
  waits for Start and exits 0 without executing anything. Its emptyDir
  output is invisible to VM guests (mounts are no-ops there and the shim's
  injection already staged execd/bootstrap into the rootfs), so the
  throwaway microVM it would otherwise boot — sequentially, on every
  sandbox creation — buys nothing. Container-tier pods run it normally:
  there the emptyDir copy is the execd delivery mechanism.

## Pod-level sandboxes

The container-tier path implements pod-level sandboxes: Create/Start/Wait/Stop/Shutdown/
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
- Nonempty AppArmor/SELinux process labels, user names, scheduler, I/O
  priority and CPU affinity settings are rejected for both init and exec
  processes. `oomScoreAdj` is enforced (written before the uid switch);
  kubelet sets it on every pod container.
- Host cgroup handling implements `pids.limit`, `memory.limit`,
  `cpu.quota`/`cpu.period`, `cpu.shares` (as v2 weight), `memory.swap`
  (runc's v2 mapping: the excess over the memory limit becomes
  `memory.swap.max`) and the kubelet cgroup-v2 `unified` keys
  `memory.swap.max`/`memory.oom.group` — every other `unified` control is
  rejected, and so are swap reservations, cpuset, block IO, huge pages,
  network and RDMA. A usable cgroup path and write permissions are
  required; setup errors fail task creation. Creating a leaf cgroup
  enables the needed controllers on its parent's `cgroup.subtree_control`
  (what runc's systemd driver otherwise does). Systemd-style
  `slice:prefix:id` paths translate to the slice's parent chain plus one
  `<prefix>-<id>.scope` entry — the layout kubelet's systemd cgroup driver
  produces — **not** delegated through systemd.
- Stats, pause/resume, and checkpointing are unimplemented.
- VM exec: no tty, and the process starts at `Exec` time (see above).
- Kubelet's cgroup mount arrives typed `cgroup` even on cgroup-v2-only
  kernels; it is mounted as the unified hierarchy (runc's mapping).
  Mountpoint preparation does not follow image-provided symlinks, so a
  volume destination under an image symlink directory (Debian-style
  `/var/run` → `/run` with the serviceaccount token) fails task creation
  until runc-securejoin-style resolution is implemented; pods can set
  `automountServiceAccountToken: false` to avoid that path.

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

VM init and exec accept OCI process identity fields (uid/gid, supplementary
groups, umask), capability sets and `noNewPrivileges`: they describe host
processes, which the VM boundary subsumes (the same treatment kata and
firecracker apply). They are not installed inside the guest. Init and exec
rlimits travel to the guest supervisor/agent and are applied with
`prlimit64` before the workload runs. Neither the VM boundary nor VM shape
substitutes for a requested OCI process security policy.

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

The Linux syscall paths can be compile-checked from a development host:

```bash
just check -p pvisor-shim --target x86_64-unknown-linux-musl
just lint-rust -p pvisor-shim --target x86_64-unknown-linux-musl
```

Build selection uses the same `just build PROFILE COMPONENT` entry as the CLI
and daemon. Profiles are `debug`, `release`, and `performance`; `shim` builds
the host-only binary without preparing firmware, while `shim-vm` enables `vm`
and shares firmware resolution, guest compilation, Zig linking and artifact
publication with the other native components. Both publish
`target/PROFILE/containerd-shim-pvisor-v2` (or under `CARGO_TARGET_DIR`); building
another variant replaces that binary. The default `all` selection includes the
VM-enabled shim on Linux, alongside all other supported workspace executables.
The explicit `product` selection retains the CLI/daemon wheel set; the wheel's
contents are independent of the full development build.
Cross-building the host-only shim needs the Linux musl Rust target, Zig and
cargo-zigbuild; executing it requires Linux. VM cross-builds additionally need
the firmware override described above.

On Linux, `just test shim -- --features vm` also tests the VM-enabled code.
CI builds and tests both shim variants using the shared native-component matrix,
and runs host-independent shim tests in the macOS shard. These are conventional
tests, not containerd or real-KVM acceptance.

## Linux acceptance smoke test

On a Linux host with containerd ≥ 1.7, build the host-only shim:

```bash
just build release shim
sudo install -m755 target/release/containerd-shim-pvisor-v2 /usr/local/bin/
sudo systemctl restart containerd
sudo ctr run --runtime io.containerd.pvisor.v2 -t --rm \
  docker.io/library/busybox:latest pvisor-smoke echo hello from pvisor
```

For VM support, replace the build command with
`just build release shim-vm` and install
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
