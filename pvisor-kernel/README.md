# pvisor-kernel: Rust native kernel

An experimental second guest kernel alongside the Linux firmware in `../fw`.
The selected kernel is compiled directly from the trimmed source in this
directory. No kernel repository is cloned or downloaded during a build.
The pVisor VMM, virtio-MMIO devices, virtio-fs root filesystem and guest
supervisor remain the execution architecture. Linux remains the default.

## Source layout

- `kernel/`: kernel assembler, syscall/VFS/process implementation and selected drivers.
- `ostd/`: low-level Rust CPU, memory and interrupt framework.
- `osdk/`: local kernel build tooling and allocators.
- `vdso/`: pinned x86_64 and aarch64 userspace ABI helpers.
- `IMPORT.md`, `IMPORT-files.json`, `provenance.json`: original source attribution,
  imported-file hashes and local changes.

ext2/exFAT disk filesystems, DRM/simpledrm, i8042, NVMe, MLS disk,
virtio block/input drivers, RISC-V/LoongArch
architectures, upstream CI/distribution and upstream branding assets are not
imported. Generic PCI, framebuffer and input abstractions remain where shared
code still depends on them. Legacy PCI probing is disabled on this MMIO profile.
Confidential-guest default features are disabled.
Root filesystem boot accepts only the `/dev/root` virtio-fs tag and `virtiofs`
filesystem type; other root devices or unsupported filesystem candidates fail.
Pseudo-filesystems and the in-memory bootstrap filesystem remain available.

Upstream dependency names, URLs, copyright and license notices retain their
real attribution. The kernel package, runtime selection and diagnostics use
the Rust native kernel identity.

## Build

On Linux or macOS, selecting the host architecture by default:

```sh
mise run kernel -- build --prepare-toolchain -j4
# Cross-build the ARM64 guest on either host architecture:
mise run kernel -- build --architecture aarch64 --prepare-toolchain -j4
```

The first invocation installs the pinned Rust nightly inside
`target/pvisor-kernel/rustup/`, not the user's Rust installation. Later builds
can omit `--prepare-toolchain`. Rust and Cargo dependency acquisition still
need network access on the first build. `--offline` requires those dependencies
to be available locally.

Inputs are snapshotted into a fresh content-addressed build directory.
The command prints a bundle directory containing `kernel.bin`, `kernel.json`
and `pvisor-kernel.SOURCE`. The receipt includes source, toolchain, dependency
lock and artifact hashes. Compilation is independent of the main Rust workspace.

## Experimental selection

Linux x86_64/aarch64 and Apple Silicon macOS product builds (CLI, daemon and wheels) compile
and embed this payload alongside the default Linux firmware. Both use the
existing compressed in-executable kernel storage. No separate kernel files
are installed or needed at runtime.

```sh
export PVISOR_NATIVE_KERNEL=rust
pvisor vm-run --rootfs /absolute/path/to/disposable/rootfs -- /absolute/guest/program
```

Selection never downloads, compiles or reads an external kernel at runtime.
Unset `PVISOR_NATIVE_KERNEL` to return to Linux. If a standalone Cargo
build did not embed this payload, selection fails rather than reading a bundle
or falling back to Linux.

The build script validates the metadata, boot addresses, size and SHA-256
before embedding bytes. Product builds always prepare both kernels; there is
no user-facing bundle-path configuration and no build-time kernel choice.
The runtime selector does not affect which payloads are included in a product.
The combined product firmware receipt retains both payloads' source identities.
`--firmware` cannot override the Rust native payload; it is rejected for this
selection instead of loading a file or falling back to Linux.

The current profile selects Linux x86_64/aarch64 KVM or macOS aarch64 HVF,
with a fresh single-vCPU VM and at least 256 MiB
RAM, without EFI, TEE, snapshots, restore or Linux idle handoff.
ARM64 uses the existing RAM base `0x80000000`, an Image/header entry at
`0x80200000`, FDT in `x0`, GICv3, the architectural timer and PSCI.
The Image header branches to `_start`; entering the ELF `_start` directly
does not establish the link register required by its load-address check.
The VMM copies the image into ordinary guest RAM and reserves the FDT tail.
Linux ARM KVM hosts without GICv3 fail explicitly for this kernel.
The driver profile includes only the primary VM console. Named task stdio ports are not
available, so workflows requiring `task_stdio`, including ordinary captured
container execution, return `Unsupported`. Multiport character devices and
their sysfs names must be implemented before enabling those workflows.
Do not treat this as Linux syscall or workload compatibility.

## Tests and hardware acceptance

Current status: Rust native kernel builds and embeds, but its complete Linux/KVM
workload acceptance test still times out during primary-console initialization.
The aarch64 source and cross-build path are restored; ARM KVM/HVF workload
acceptance requires an ARM host and has not been run on the x86_64 development host.
The ARM64 boot-only QEMU/TCG diagnostic passed through kernel-thread startup
and the expected missing-rootfs failure. The ARM Linux VMM library cross-check
and CLI cross-check also passed with the actual ARM payload embedded.
Common filesystem records and control framing compile independently of
backend-only checkpoint/restore code. The macOS VMM cross-check
on Linux is blocked by the missing Darwin FUSE SDK; a native macOS build is
still required.
Its shared-rootfs command execution and authenticated guest exit have not passed
end-to-end acceptance. Packaging and runtime selection are implemented; the
experimental payload is not yet a usable replacement for Linux task execution.

```sh
mise run kernel -- test
mise run test -- pvisor-vm
mise run test-doc -- pvisor-vm
```

Tests cover source trimming, local build inputs, ELF layout, selector
validation and bundle integrity. They also check that repository ignore rules
do not exclude build inputs, including the OSDK's source directory named `build`.
Clean-source build validation must use a Git export rather than local untracked files.
Real VM acceptance additionally requires
shared-rootfs reads/writes, the unmodified guest supervisor and authenticated
exit codes. A boot or successful compilation alone does not meet that bar.

An ARM64 boot-only diagnostic can run on an x86 host with `qemu-system-aarch64`:

```sh
python3 pvisor-kernel/tests/boot_arm.py --qemu /path/to/qemu-system-aarch64 \
  --bundle /absolute/path/printed/by/the/aarch64/build
```

It checks CPU/MMU/FDT/GICv3 initialization, the first kernel thread and PSCI
shutdown after the expected missing-virtio-fs-root error. It uses QEMU's FDT
and an entry trampoline, not the pVisor VMM. It supplies no rootfs, does not
execute a workload and does not establish KVM/HVF compatibility.

A static hardware workload is provided in `tests/guest.rs`:

```sh
mise exec -- rustc --edition=2021 --target x86_64-unknown-linux-musl \
  -C linker=rust-lld pvisor-kernel/tests/guest.rs -o target/native-kernel-smoke-guest
```

Copy it to a fresh disposable rootfs as `/probe` and run it with `vm-run`
and argument `7` under a caller-owned deadline. It checks file write/read,
stdout/stderr emission and rejection of forged supervisor ioctls from a
non-PID-1 process. The expected authenticated exit code is 7.

## License

This imported source is primarily MPL-2.0, not Apache-2.0. See `LICENSE-MPL`,
`.licenserc.yaml`, per-file notices and `IMPORT.md`. Keep modified source and
applicable license notices with redistributed kernels. The pinned Linux vDSO
also requires its matching source and notices before redistribution; the
current binary input archive does not supply them.
