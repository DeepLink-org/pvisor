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
export PVISOR_GUEST_KERNEL=rust-native
```

Selection applies to the existing Docker-compatible and Agent VM workflows,
not a separate public execution command.

Selection never downloads, compiles or reads an external kernel at runtime.
Unset `PVISOR_GUEST_KERNEL` to return to Linux. If a standalone Cargo
build did not embed this payload, selection fails rather than reading a bundle
or falling back to Linux.

The build script validates the metadata, boot addresses, size and SHA-256
before embedding bytes. Product builds always prepare both kernels; there is
no user-facing bundle-path configuration and no build-time kernel choice.
The runtime selector does not affect which payloads are included in a product.
The combined product firmware receipt retains both payloads' source identities.
`--firmware` cannot override the Rust native payload; it is rejected for this
selection instead of loading a file or falling back to Linux.

The profile requires a fresh VM without EFI, TEE, snapshots, restore or Linux
idle handoff. Linux x86_64/KVM accepts **1–64 vCPUs**, with at least **16 MiB**
or **1.5 MiB per vCPU rounded up**, whichever is larger. That is 16 MiB for
1–10 vCPUs, 17 MiB for 11, 24 MiB for 16, 48 MiB for 32 and 96 MiB for 64.
These are launch limits, not a guarantee that an arbitrary workload fits.
The MADT lists each configured vCPU; the kernel reports the scheduling CPU
through both `getcpu(2)` and the Linux vDSO.
Linux aarch64/KVM and macOS aarch64/HVF still require one vCPU and at least 256 MiB.
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

The [inline metadata path](../crates/pvisor-vm/README.md#virtio-fs-并发与冻结契约)
negotiates device feature bit 23. An idle ordinary queue can use bit 31 in its
MMIO notification value for `LOOKUP` or `GETATTR`. The guest checks the used
ring on return and waits for the ordinary IRQ if the worker owns the request.
It defaults on for eligible single-queue shares on Linux x86_64/KVM.
Set `PVISOR_VM_FS_INLINE_METADATA=0` to disable it.

Path-based `stat`, `lstat` and `newfstatat` can reuse the last component's
`LOOKUP` attributes when that walk obtained them for the resolved inode.
This does not extend TTLs or change directory search checks. `statx` and
descriptor-only queries keep their existing paths.

On staged OverlayFS with explicitly immutable lowers and a guest-exclusive
upper, virtio-fs can import positive entries with the Linux firmware's
`READINDEX` protocol when DAX and host writeback are off.
Four distinct successful host lookups in a directory
admit a 16 KiB page. A hit and at least three quarters consumption can admit
one continuation of up to 512 KiB. Each mount retains at most one such image.
Already imported entries can revalidate matching live dentries in the same
directory, credential context and request generation. Guest requests that
may change names or attributes invalidate that generation. Directory search
permissions still run through VFS; unimported names, negative lookups and
userspace enumeration use ordinary FUSE. Imported attributes do not extend
ordinary inode TTLs. Set `PVISOR_VM_FS_RNGK_READINDEX=0` to disable the
default-on path. `PVISOR_VM_GUEST_CACHE=0` also disables the protocol.

## Tests and hardware acceptance

On Linux x86_64/KVM `vm_api_case` runs the guest supervisor and a static
workload on a writable virtio-fs root. The workload checks the boot ID, reads
back its write, rejects forged exit ioctls, and exits with code 7.
The aarch64 source and cross-build path are restored; ARM KVM/HVF workload
acceptance requires an ARM host and has not been run on the x86_64 development host.
The ARM64 boot-only QEMU/TCG diagnostic passed through kernel-thread startup
and the expected missing-rootfs failure. The ARM Linux VMM library cross-check
and CLI cross-check also passed with the actual ARM payload embedded.
Common filesystem records and control framing compile independently of
backend-only checkpoint/restore code. The macOS VMM cross-check
on Linux is blocked by the missing Darwin FUSE SDK; a native macOS build is
still required.
These VM API probes do not qualify the Docker CLI workload matrix or ARM guest
execution.

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
For the buffered-read regression, prepare a 4 MiB `/read-fixture` with
byte `i` equal to `(i / 4096 + i % 251) % 256` (integer division) and
append `--check-read-fixture` after `7`. The workload warms one page,
verifies an unaligned multi-batch read spanning it, and checks the short
read at EOF. See the [filesystem A/B](../benchmark/evidence/rust-kernel-filesystem.md).

`--check-write-cache` instead tests sparse write-only I/O, a subsequent
buffered read or shared mapping, and truncate/regrow. After exit, `/mapped`
and `/buffered` must each contain 8192 zero bytes followed by `78 00 00`
(hex). The guest also tests set-ID bits and file-capability removal on its
own tmpfs before and after dropping effective `CAP_FSETID`.

On virtio-fs, a regular file starts without a page cache. Writes use the
caller's file handle and wait for the server reply; the first buffered read
or mapping creates the cache under the same inode lock as writers. From
then on, buffered writes keep it coherent. This is not writeback caching.
See the [write-path experiment](../benchmark/evidence/rust-kernel-write-path.md).

When the server returns `FOPEN_KEEP_CACHE`, closing a cached file still
flushes dirty pages but retains clean pages. Opens without the flag still
invalidate cached data. To verify copy-up across this boundary, start the
smoke guest with `7 --check-keep-cache-copy-up` on a staged OverlayFS root
whose immutable lower contains `/keep-cache-source` (8192 `a` bytes).
Expect exit 7, an unchanged lower, and an upper with `new` at offset 4095.

For ordinary writes, virtio-fs can reuse the absence of set-ID bits until
the server's original attribute deadline. Writes expire general attributes
without extending that deadline. A mode change refreshes or invalidates the
cached result; an expired result and files with set-ID bits keep the normal
privilege checks.

`syncfs` sends `FUSE_SYNCFS` and propagates backend errors to the caller and
the guest supervisor's exit check. A legacy server's `ENOSYS` retains the
write-through behavior and is not a durable-stage acknowledgement.

The [write-path probe](tests/write_path.rs) runs as UID/GID 1000 without
effective capabilities. It tests set-ID clearing after `chmod`, then writes
4 MiB in 1 KiB calls. `shared` writes and publishes on virtio-fs; `local`
writes on guest tmpfs and exports one new file to virtio-fs in larger chunks.
The probe reports separate write, local completion and publication times.
It does not provide an overlay importer or crash recovery for guest RAM.

```sh
root=$(mktemp -d "$PWD/target/write-path-root.XXXXXX")
mkdir -m 0777 "$root/output"
mise exec -- rustc --edition=2021 --target x86_64-unknown-linux-musl \
  -C linker=rust-lld -O pvisor-kernel/tests/write_path.rs -o "$root/probe"
PVISOR_GUEST_KERNEL=rust-native timeout 30 /path/to/vm_api_case \
  --rootfs "$root" --cpus 1 --memory 512 -- /probe shared
"$root/probe" --verify "$root/output/ready"
```

Use a new root with `local` for the comparison. Neither arm counts VM
startup, exit or the separate host byte verification in its phase times.

The [directory probe](tests/readdir.rs) checks short buffers, retry after
`EINVAL`, resume from a directory cookie, rewind, EOF and exact names across
several buffer sizes. Rust native virtio-fs sizes `READDIR` from the caller's
buffer, between one page and 32 KiB, without caching directory entries across
calls. To run it with a native-enabled `vm_api_case`:

```sh
root=$(mktemp -d "$PWD/target/readdir-root.XXXXXX")
mkdir "$root/readdir-fixture"
python3 - "$root/readdir-fixture" <<'PY'
from pathlib import Path
import sys

directory = Path(sys.argv[1])
for index in range(8192):
    (directory / f"entry-{index:05}").touch()
PY
mise exec -- rustc --edition=2021 --target x86_64-unknown-linux-musl \
  -C linker=rust-lld -O pvisor-kernel/tests/readdir.rs -o "$root/probe"
PVISOR_GUEST_KERNEL=rust-native timeout 30 /path/to/vm_api_case \
  --rootfs "$root" --cpus 1 --memory 512 -- /probe
```

Expect exit 0 and four `READDIR_OK` records. The reported time covers one
resident scan and decoding, not VM startup, close or exit.

To verify captured streams on Linux x86_64/KVM, run the same static workload
with the embedded-kernel probe:

```sh
python3 pvisor-kernel/tests/task_stdio.py --binary /path/to/vm_api_case \
  --guest target/native-kernel-smoke-guest
```

This checks immediate EOF and a multi-page binary input, byte-exact stdout,
separate stderr, shared-rootfs contents and exit status in fresh two-vCPU VMs.

### KVM metadata acceptance

On Linux x86_64/KVM, validate `newfstatat` against host mutations in the
default mode and with the inline path disabled:

```sh
cc -static -O2 pvisor-kernel/tests/stat_visibility.c -o target/native-stat-visibility
python3 pvisor-kernel/tests/stat_visibility.py --binary target/release/examples/vm_api_case \
  --guest target/native-stat-visibility
```

CI runs the same checks and verifies that the default and disabled modes
use different paths. To reproduce the full check with native-enabled
`vm_api_case` and `guest_metadata_case`:

```sh
mise exec -- rustc --edition=2021 --target x86_64-unknown-linux-musl -C linker=rust-lld -O \
  crates/pvisor-vm/src/probes/guest_metadata_guest.rs -o target/metadata-guest
python3 pvisor-kernel/tests/fs_metadata.py \
  --vm-api target/release/examples/vm_api_case --stat-guest target/native-stat-visibility \
  --metadata-case target/release/examples/guest_metadata_case --metadata-guest target/metadata-guest \
  --output target/native-fs-metadata --seed 1 --seed 314159 --seed 57596
```

Use a fresh output directory per run. The staged probe checks paged lookup,
rename over an existing entry, an open fd across rename, deletion and recreation,
copy-up, and non-root directory search. Each `--seed` adds a fresh two-vCPU VM
with two request queues, concurrent stats and seeded namespace changes. It
checks old descriptors and final file state. The seed controls the operations,
not thread scheduling.
These are correctness checks, not timing gates.

### KVM memory acceptance

Run on Linux x86_64 with KVM, host KSM advice support, and permission to
read `/proc/<child>/mem` and `/proc/<child>/pagemap` for child runners.
Use a directory on a disk filesystem for `--backing-dir`; tmpfs cannot
demonstrate a reduction in file-backed residency.

```sh
# Set to the bundle directory printed by `mise run kernel -- build`.
export PVISOR_BUILD_NATIVE_KERNEL_BUNDLE=/absolute/path/to/bundle
mise exec -- cargo build --locked -p pvisor --example native_memory_case
mise exec -- rustc --edition=2024 --target x86_64-unknown-linux-musl \
  -C linker=rust-lld -C opt-level=2 pvisor-kernel/tests/memory_guest.rs \
  -o target/native-memory-guest
python3 pvisor-kernel/tests/memory.py --binary target/debug/examples/native_memory_case \
  --guest target/native-memory-guest --backing-dir target
```

Repeat with `--firmware /absolute/path/to/libkrunfw.so.5` to run the same
memory checks on the Linux guest. Use the path printed by `mise run fw -- build`.

The probe checks KSM advice and guest readback without claiming KSM merges or
changing the host scanner. It measures host residency before and after manual
file-backed offload, then verifies the same workload's bytes after resume. Two
separate VMs must map identical workload pages to the same physical-pool file
slots before one writes. The writer verifies its changes, the other VM verifies
the original bytes, and both must exit through the guest supervisor.

These are host memory mechanisms. Linux guest-idle automatic offload requires
an interrupt-safe idle offer and timer deadline from the guest. The native
kernel does not supply that protocol. Snapshot/restore COW is also unsupported;
the physical pool tests live sharing, not snapshot-backed cloning.

### x86_64 vCPU/RAM matrix

Build the native payload and the VM API probe, then run the matrix on a host
with `/dev/kvm`. Use the static guest compiled above:

```sh
export PVISOR_BUILD_NATIVE_KERNEL_BUNDLE="$(mise run kernel -- build -j4)"
mise exec -- cargo build --locked -p pvisor-vm --example vm_api_case
python3 pvisor-kernel/tests/resource_matrix.py \
  --binary target/debug/examples/vm_api_case \
  --guest target/native-kernel-smoke-guest --repeat 3
```

The matrix covers odd CPU counts, the 16→17 MiB admission step, and up to
64 vCPUs. Each case checks that the initial affinity mask contains exactly the
configured CPUs and that concurrent userspace threads actually execute on
every vCPU. It also checks named stdio, shared-rootfs contents and authenticated
exit status. Use `--case VCPUS:MIB` (repeatable) to target particular pairs.
Each run gets a fresh rootfs and has a 30-second deadline; increase `--timeout`
on hosts with fewer physical CPUs.

On an AMD Ryzen 7 9700X KVM host, the matrix passed at 10 vCPUs with 16 MiB
and 64 vCPUs with 96 MiB. Alpine through `pvisor run` also reported the
configured CPU count at both sizes. With admission bypassed
for diagnosis, a single-vCPU probe passed at 8 MiB but the image did not fit
at 7 MiB. Before boot-stack reclamation, a 16-vCPU boot at 16 MiB exhausted
the kernel heap. These observations do not extend the supported sizes.

The x86 image starts at 2 MiB rather than 128 MiB. The guest uses 64 KiB task
stacks and caps the two virtio-fs DMA arena reservations together at one eighth
of usable RAM (at most 16 MiB total). If the optional arenas cannot be reserved,
large transfers use per-request DMA buffers. Each AP releases its boot stack
after switching to its first task. Startup still needs all of those stacks
and the per-CPU kernel workers still consume RAM as vCPU count rises.

## License

This imported source is primarily MPL-2.0, not Apache-2.0. See `LICENSE-MPL`,
`.licenserc.yaml`, per-file notices and `IMPORT.md`. Keep modified source and
applicable license notices with redistributed kernels. The pinned Linux vDSO
also requires its matching source and notices before redistribution; the
current binary input archive does not supply them.
