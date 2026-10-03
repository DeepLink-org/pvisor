# libkrun VM

The VM executor boots a minimal Linux guest through libkrun, giving it an independent guest kernel, a workspace served over virtio-fs, and networking handled by the pVisor smoltcp data plane. Linux uses KVM; Apple Silicon macOS uses HVF.

```bash
pvisor run --executor vm --rootfs image=ubuntu:24.04 \
  --stage ../stage-vm --mount "$PWD:stage" -- /bin/sh
```

## Prepare a rootfs

| Source | Option | Behavior |
| --- | --- | --- |
| OCI image | `--rootfs image=<IMAGE>` | Pulls the image directly without Docker, Podman, or Buildah; verifies the manifest and layer digests and selects `linux/arm64` or `linux/amd64` to match the host architecture |
| Prepared directory | `--rootfs <PATH>` | Uses an existing Linux rootfs |
| Host root, Linux only | `--rootfs host` | Uses the host `/` as a read-only lower layer and keeps the host PATH and HOME; it exposes host content to reads |

Pin image digests when you need reproducibility. `--image-store` overrides the image cache directory; when several VMs share image files, use the [shared image cache](../../reference/shared-image-cache.md). On macOS you must provide a Linux rootfs or image explicitly.

## Where writes go

- The merged rootfs is the guest `/`, and `/workspace` is the guest's working directory.
- Workspace changes go to the chosen stage or the default Job storage and survive exit for review and apply.
- Other writes to the VM root use a temporary upper that is discarded when the VM exits.
- The image cache is marked immutable; `apply` cannot rewrite a rootfs shared with other Runs.

## Networking

| Mode | Behavior |
| --- | --- |
| `--overlaynet auto` (default) | Static guest IPv4 addresses, synthetic DNS, and policy-controlled IPv4 TCP; the guest has no direct network bypass |
| `--overlaynet off` | No guest networking; offline |

Unsupported UDP, IPv6, ICMP, QUIC, and inbound forwarding fail closed. `deny-all` still allows configured internal Gateway routes; for full offline operation, disable the Gateway and use `off`. The VM does not support `proxy` mode.

## Platform setup

- **Linux**: needs an accessible `/dev/kvm`; the static musl build embeds the guest kernel, so no firmware shared libraries are needed at run time.
- **macOS**: build and sign the Hypervisor entitlement with `just build release`; the wheel bundles `libkrunfw`, and running from source downloads a pinned version verified by SHA-256.

## Known gaps

- On Linux the VMM is additionally confined with namespaces and Landlock; **on macOS the VMM still holds the calling user's host permissions**, so despite guest kernel isolation it is not a hostile multi-tenant boundary.
- Host connectors and shared files remain part of the boundary.
- `--safe` requires the VM to use the existing `auto` network boundary, but it does not select the VM automatically.

## Memory options

`--memory` sets guest RAM capacity, not actual physical occupancy. On macOS / Apple Silicon, explicitly select `--vm-memory-pool SOCKET` to share immutable compressed cold blocks; it defaults off, and pool loss fails attached VMs. `--vm-ram-backing FILE` selects a separate new RAM file and does not itself guarantee savings. `--vm-ram-compression` uses the FUSE Seekable compression path and is mutually exclusive with the pool. Read the [experiments and usage decisions](../../benchmarks/vm-memory/index.md) before enabling it.
