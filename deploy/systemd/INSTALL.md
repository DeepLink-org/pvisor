# Native daemon user-service example

The daemon is supported on **Linux x86_64 only**, with usable `/dev/kvm` and
cgroup v2. The static musl binary embeds pVisor's VM library, Rust guest supervisor
and libkrunfw kernel; it needs neither Podman nor a host `libkrun.so`/`libkrunfw.so`.
`libkrunfw.SOURCE`, `LICENSE` and `NOTICE` accompany the standalone distribution.
Building from source uses `just daemon-build release` and the shared Zig/firmware
preparation pipeline, not a firmware-free Cargo build.

This is an operator-configured example, **not a validated end-to-end installation**.
The prepared-image bootstrap is not supplied. The daemon main/Cargo integration
must match this native runtime; a binary still using the retired Podman adapter
cannot use this unit.

## Host preparation

1. Install `pvisor-daemon` at `~/.local/bin/pvisor-daemon` and the service at
   `~/.config/systemd/user/pvisor-daemon.service`.
2. Give the service user access to `/dev/kvm` using the host's device policy.
3. Provision a **separate, existing delegated cgroup v2 subtree** writable by
   that user. Enable `cpu memory pids` in its `cgroup.subtree_control`, ensure
   child groups support `cgroup.kill`, and configure its absolute path as
   `PVISOR_DAEMON_CGROUP_ROOT`. The daemon checks real controller writes and KVM.
   `Delegate=cpu memory pids` alone neither creates this path nor enables its
   controllers. Do not use an ordinary directory or a populated service cgroup
   as a substitute: cgroup v2's no-internal-process rule still applies. Maintain
   this delegation and path across daemon restarts; host administrators own its
   provisioning and teardown.
4. Provision a private, user-owned mode-0700 persistent state directory, for
   example `/var/lib/pvd`, and set `PVISOR_DAEMON_STATE`. Use a unique short path
   for each daemon. All paths must be absolute. Per-sandbox `control.sock` paths
   must be **shorter than 104 bytes**, and native vsock Unix sockets also have
   path limits. Long home-directory paths can fail after admission. Do not erase
   state on restart or use a temporary runtime directory if reboot persistence
   is required. VMs cannot survive a host reboot; retained ownership records
   remain necessary for reconciliation.
5. Provision trusted local image manifests in `PVISOR_DAEMON_IMAGES_DIR`
   (example `/srv/pvi`). Each `<key>.json` supplies absolute `rootfs`, absolute
   guest `entrypoint` argv and optional `cmd`, `env`, `library_dir`. The rootfs
   must be independently provisioned, immutable, not host `/`, and must not
   overlap daemon state. The bootstrap must supervise and initialize real
   OpenSandbox **1.1.0 execd/egress** and the workload, forward signals, reap
   children, and expose byte-transparent AF_VSOCK listeners on **CID 3**, ports
   **44772/18080**. A stock rootfs or upstream registry image name is insufficient.
6. Copy `daemon.env.example` to `~/.config/pvisor/daemon.env`, set mode 0600,
   supply the cgroup path and a strong random `OPEN_SANDBOX_API_KEY` of at least
   32 bytes, and check the other settings before enabling the user service.

Use `systemctl --user daemon-reload` then `systemctl --user enable --now
pvisor-daemon.service` only after those prerequisites are ready. User-manager
lifetime/linger is host policy, not guaranteed by this unit. Keep loopback binding;
external access requires a trusted TLS proxy and an explicit `--public-endpoint`
unit override. Guest service authentication is required even for local ports.

## Restart and retirement

`KillMode=process` intentionally leaves detached supervisors alive on daemon
stop/restart. They retain kernel-enforced sandbox cgroup limits; normal daemon
shutdown is not sandbox deletion. Preserve the state, images and delegated root
for restart. Before retiring a deployment, explicitly delete sandboxes through
the API and verify cleanup; do not remove state or delegation as a shortcut.
TTL cleanup is best effort while the daemon is running. This unit does not
provide cleanup while the daemon is down.

This deployment does not implement daemon stage/apply, checkpoints, requested
network policies or automatic node-resource sharing, and is not evidence of
SDK conformance, native isolation testing or sandbox density.
