# libkrun VM

VM execution boots a small Linux guest through libkrun, with independent kernel, virtio-fs workspace, and pVisor smoltcp networking. Linux uses KVM; Apple Silicon macOS uses HVF.

```bash
pvisor run --executor vm --rootfs image=ubuntu:24.04 \
  --stage ../stage-vm --mount "$PWD:stage" -- /bin/sh
```

## Prepare a rootfs

| Source | Option | Behavior |
| --- | --- | --- |
| OCI image | `--rootfs image=IMAGE` | Pull directly without Docker/Podman/Buildah; verify manifests/layers; select host-matching linux/arm64 or linux/amd64 |
| Prepared directory | `--rootfs PATH` | Use an existing Linux rootfs |
| Host root, Linux only | `--rootfs host` | Host / as read-only lower with host PATH/HOME; exposes host content for reads |

Pin image digests for reproducibility. `--image-store DIR` overrides cache location; [shared cache](../../reference/shared-image-cache.md) can serve multiple VMs. macOS requires explicit Linux rootfs/image.

## Write destinations

- Merged rootfs is guest /; working directory is /workspace.
- Workspace writes remain in the configured stage/default storage for review/apply.
- Other guest-root writes use temporary upper discarded on exit.
- Image cache is immutable; apply cannot mutate shared rootfs lowers.

## Network

| Mode | Behavior |
| --- | --- |
| auto (default) | Static IPv4, synthetic DNS, policy-controlled IPv4 TCP; no guest bypass |
| off | No guest networking |

Unsupported UDP, IPv6, ICMP, QUIC, and inbound forwarding fail closed. Deny-all still permits configured internal Gateway routes. For complete offline execution disable Gateway and use off. VM rejects `--overlaynet proxy`; use `--overlaynet off` for offline execution.

## Platform setup

- Linux needs accessible /dev/kvm. Static musl builds embed the guest kernel without runtime firmware libraries.
- macOS source builds use just build release for Hypervisor signing. Wheels include libkrunfw; source execution downloads a pinned SHA-256-verified version.

## Gaps

- Linux confines VMM with namespaces/Landlock. macOS VMM retains the caller's host permissions; guest isolation does not establish hostile multi-tenancy.
- Host connectors and shared files remain within the trust boundary.
- `--safe` requires `--overlaynet auto` networking but does not select VM automatically.
