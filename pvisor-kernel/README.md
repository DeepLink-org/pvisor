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

The kernel shares the main workspace's dated Rust nightly and selected Rustup
home (`RUSTUP_HOME`, when set). `--prepare-toolchain` installs the additional
kernel components and bare-metal target there; toolchain installation is not
stored inside Cargo's `target/` cache. Later builds can omit this flag.
Rust and Cargo dependency acquisition still
need network access on the first build. `--offline` requires those dependencies
to be available locally.
The builder resolves that nightly's executables and places them before host
tools for OSDK and its child processes; a stable Cargo from mise or `PATH`
cannot select the kernel compiler.

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
```

Selection applies to the existing Docker-compatible and Agent VM workflows,
not a separate public execution command.

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
with a fresh VM and at least 256 MiB
RAM, without EFI, TEE, snapshots, restore or Linux idle handoff.
ARM requires a single vCPU. The x86_64 profile supports one or two vCPUs;
the MADT lists each configured vCPU.
ARM64 uses the existing RAM base `0x80000000`, an Image/header entry at
`0x80200000`, FDT in `x0`, GICv3, the architectural timer and PSCI.
The Image header branches to `_start`; entering the ELF `_start` directly
does not establish the link register required by its load-address check.
The VMM copies the image into ordinary guest RAM and reserves the FDT tail.
Linux ARM KVM hosts without GICv3 fail explicitly for this kernel.
Captured task streams use raw virtio-serial character devices, separate from
the primary TTY. PID 1 discovers their names under `/sys/class/virtio-ports`.
The guest supervisor can configure the virtio Ethernet device's IPv4 address,
netmask and on-link default gateway through socket ioctls. These operations
require `CAP_NET_ADMIN`; arbitrary route entries and device selection via
`SIOCADDRT` are unsupported.
`/proc/net/tcp` exposes IPv4 listeners; it does not enumerate established
connections or provide real queue and timer statistics.
Do not treat this as Linux syscall or workload compatibility.

## Tests and hardware acceptance

On Linux x86_64/KVM the `vm_api_case` probe with one or two vCPUs runs the guest supervisor
and a static workload on a writable virtio-fs root. The workload checks the boot
ID, reads back its write, rejects forged exit ioctls, and exits with code 7.
This probe uses the primary console, not named task stdio.
The aarch64 source and cross-build path are restored; ARM KVM/HVF workload
acceptance requires an ARM host and has not been run on the x86_64 development host.
The ARM64 boot-only QEMU/TCG diagnostic passed through kernel-thread startup
and the expected missing-rootfs failure. The ARM Linux VMM library cross-check
and CLI cross-check also passed with the actual ARM payload embedded.
Common filesystem records and control framing compile independently of
backend-only checkpoint/restore code. The macOS VMM cross-check
on Linux is blocked by the missing Darwin FUSE SDK; a native macOS build is
still required.
The primary-console result does not qualify the Docker CLI workload matrix
or ARM guest execution.

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

Copy it to a fresh disposable rootfs as `/probe` and use the test-only
`vm_api_case` example with argument `7` under a caller-owned deadline.
The probe must be built with the native payload embedded for native selection.
It checks file write/read,
stdout/stderr emission and rejection of forged supervisor ioctls from a
non-PID-1 process. The expected authenticated exit code is 7.

To verify captured streams on Linux x86_64/KVM, run the same static workload
with the embedded-kernel probe:

```sh
python3 pvisor-kernel/tests/task_stdio.py --binary /path/to/vm_api_case \
  --guest target/native-kernel-smoke-guest
```

This checks immediate EOF and a multi-page binary input, byte-exact stdout,
separate stderr, shared-rootfs contents and exit status in fresh two-vCPU VMs.

## License

This imported source is primarily MPL-2.0, not Apache-2.0. See `LICENSE-MPL`,
`.licenserc.yaml`, per-file notices and `IMPORT.md`. Keep modified source and
applicable license notices with redistributed kernels. The pinned Linux vDSO
also requires its matching source and notices before redistribution; the
current binary input archive does not supply them.
