# Rust native kernel source import

This is an in-tree, trimmed source import, not a Git submodule or a build-time
download. The upstream is Asterinas v0.19.0 at
`c4b7ed575b7b32773475b194d2ad41fb6d03765b`.
Repository: <https://github.com/asterinas/asterinas>.
Archive and toolchain identities are recorded in `provenance.json`.
`IMPORT-files.json` records hashes before and after the local source import.
These are provenance records, not an approval ledger.

Included: x86_64/aarch64 kernel, OSTD, local OSDK build tooling, allocators and shared
libraries needed for the existing virtio-MMIO/virtio-fs execution model.
Excluded: RISC-V/LoongArch kernel architectures, DRM/simpledrm, i8042, NVMe, MLS disk,
virtio block/input drivers, ext2/exFAT disk filesystems, upstream CI, distribution, application tests and
the upstream logo. Generic input/framebuffer/PCI abstractions remain where
the selected kernel still uses them.

Local changes:

- Use the pVisor Rust native kernel identity and package name `pvisor-kernel`.
  Upstream URLs, copyright and license notices retain their actual attribution.
- Boot from the existing `/dev/root` virtio-fs tag without a block rootfs.
- Remove ext2/exFAT implementations and registration; reject block-device root
  selection instead of retaining a second root filesystem boot path.
- Derive FUSE request credentials from kernel tasks; forward only the existing
  PID-1 claim/report directory ioctls.
- Discover the primary console and named virtio-serial ports through control
  messages and expose raw streams through devtmpfs and sysfs.
- Disable legacy PCI probing on the MMIO-only pVisor platform.
- Disable confidential-guest defaults and repair the upstream no-feature x86
  build's conditional imports.
- Copy the dependency lock into OSDK-generated build crates and preserve the
  original ELF header when preparing a separate QEMU boot artifact.
- Restore the pinned ARM CPU/MMU, GICv3, timer, PSCI, signal and UART sources.
  Set ARM DRAM to `0x80000000` and kernel load/virtual addresses to
  `0x80200000`/`0xffffffff80200000` for pVisor's existing memory map.
  Enter the ARM64 Image header with the existing FDT, rather than the ELF entry.

Imported files retain their original MPL-2.0 or per-file licenses, including
the Apache-2.0/MIT exceptions in the component tooling. This directory is not
relicensed under the parent project's Apache-2.0 license.

The separately pinned prebuilt Linux vDSOs are retained locally for the
kernel's userspace ABI: x86_64 from Linux 6.2.10 and aarch64 from Linux 6.8.0.
The wheel build's corresponding-source archive includes the hash-pinned
Linux 6.2.10 and 6.8 source archives with their license texts, alongside the
Rust kernel input snapshot audited against the wheel receipt.
