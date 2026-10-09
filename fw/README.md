# pVisor guest firmware (libkrunfw)

```libkrunfw``` is a library bundling a Linux kernel in a dynamic library in a way that can be easily consumed by [libkrun](https://github.com/containers/libkrun).

The library exposes the kernel bytes and guest addresses through the `krunfw_get_kernel` ABI. Generic Linux x86-64 builds store long runs of padding bytes compactly and reconstruct the original image in a writable, 64 KiB-aligned mapping when the library loads. Other variants retain the flat bundle format.

## Integration in pVisor

This directory is the maintained source of pVisor's customized guest firmware,
not a nested Git repository or a Python package. See [IMPORT.md](IMPORT.md) for
its source revision and the local changes included in the import.

From the pVisor repository root:

```sh
just fw build -j4
just fw test
```

CLI, daemon and single-wheel builds use firmware compiled from this directory
by default; no prebuilt upstream firmware is downloaded by the build pipeline.
Linux x86-64 builds embed the kernel in the executable. macOS ARM64 builds ship
the built dylib beside the executables. Installing a published wheel does not
require a kernel compiler.

`fw build` uses the native host's default firmware target and supplies
`pyelftools`; the platform kernel toolchain must be installed separately.
The first build downloads the pinned Linux source archive and verifies its
SHA-256 checksum. Build snapshots and outputs live under `target/fw/` (or
`PVISOR_FW_BUILD_DIR`), not in this source directory. Sources, configurations,
patches, generator and toolchain identities determine the cache key; changed
inputs build in a fresh tree. `just fw build -j4 --offline` requires cached
verified kernel sources and locally available Python dependencies.

On macOS, install Homebrew `llvm`, `lld`, `make`, `gnu-sed` and `gnu-tar`. The pVisor
builder discovers Homebrew's prefix and prefers its LLVM/GNU tools within the
firmware build, without changing your shell's `PATH`. If Homebrew is unavailable,
it uses tools on `PATH`. On case-insensitive filesystems, it creates and mounts a
temporary case-sensitive APFS sparse image (32 GiB maximum, allocated as needed),
copies the completed firmware and actual kernel configuration back to the build
cache, then unmounts and removes the image, including after build failures.
`PVISOR_FW_BUILD_DIR` is optional; an existing case-sensitive cache builds directly.
If unmounting fails, the image is retained and the error is reported. The native
macOS kernel build path remains experimental; it does not bootstrap with a
prebuilt firmware or a VM.

An explicit `PVISOR_LIBKRUNFW_PATH` can select a library file or directory instead.
The lower-level `PVISOR_KRUNFW_PATH` accepts a library file;
`PVISOR_KRUNFW_KERNEL_BUNDLE` accepts a Linux kernel bundle directory containing
`kernel.bin` and `kernel.json`. Set at most one selector: conflicting inputs are
rejected. The selected input is resolved once and shared by compilation, payload
staging and source attribution. These overrides are optional, not required for
normal builds; `just fw build` always builds the maintained in-tree firmware.
Cross-build and SEV/TDX variants remain available through the standalone Makefile.

At runtime, the build-embedded kernel takes priority. Dynamic builds require a
local firmware in `--vm-library-dir` or beside the executable. The selected file
is canonicalized and passed to the VM runner explicitly; loader search paths are
not firmware selectors. Missing firmware is an error, never an upstream download
or runtime compilation.

Each default build retains a `libkrunfw.SOURCE` receipt with artifact, input and
actual kernel configuration hashes. To export the matching sources:

```sh
just fw build --source-output target/libkrunfw.SOURCE \
  --source-archive target/pvisor-firmware-source.tar.gz
```

Release CI publishes corresponding-source archives alongside the wheels and
daemon distribution. They contain the original kernel archive, firmware sources,
patches, licenses, actual configuration and build receipt. Extract an archive and
run the recorded Make command with `-C` changed to its `fw/` directory and `PYTHON`
changed to a local Python with `pyelftools` installed; use the recorded toolchain.
An external firmware override must supply its own corresponding sources.

For manual `make -C fw` builds only, changing patches requires a fresh Linux
source tree: the inherited Makefile applies patches at extraction time.
`make -C fw clean` removes that extracted tree and generated firmware but keeps
downloaded tarballs. Default pVisor builds handle this through isolated snapshots.

The standalone upstream GitHub workflows and CODEOWNERS were not imported;
pVisor's root CI runs the firmware bundle regression tests.

## Building

### Linux (generic variant)

#### Requirements
* The toolchain your distribution needs to build a Linux kernel.
* Python 3
* ```pyelftools``` (package ```python3-pyelftools``` in Fedora)

On Debian/Ubuntu:

```sh
apt install python3-pyelftools build-essential flex bison libelf-dev
```

#### Building and installing the library
```
make
sudo make install
```

#### Compact x86-64 kernel storage

The generic x86-64 configuration targets pVisor's virtio-mmio guest. It omits
ACPI/PCI devices, disk mapping, guest LSMs, audit, thermal management and other
unused hardware. SMP, MP tables, KVM guest support, virtio, namespaces and
seccomp remain enabled. The generic x86-64 configuration disables
`CONFIG_CPU_MITIGATIONS`, removing guest kernel CPU vulnerability mitigations.
This reduces guest protection against speculative-execution and related attacks;
host mitigations are unchanged and do not replace guest protections. These guest
mitigations cannot be re-enabled with kernel command-line parameters.

The generic x86-64 guest is IPv4-only (`CONFIG_IPV6` is disabled). IPv6 sockets,
`::1` loopback and dual-stack listeners are unavailable; workloads must support
IPv4. IPv4, virtio-net, vsock and TSI remain enabled.

For this variant, `make` builds only `vmlinux`, skipping the unused compressed
`bzImage` and its boot decompressor. The `HAVE_KERNEL_*` capability flags and
`KERNEL_GZIP` compression choice remain in the configuration as required by x86
Kconfig; they do not add a gzip decompression step to this firmware's boot path.

`make` uses `bin2cbundle.py --compact` for this variant. Literal bytes and
repeated-byte spans reconstruct the same flat kernel image: load and entry
addresses, alignment, the selected kernel's layout and all padding bytes remain
unchanged.
This reduces the library's file size; the guest image keeps its required memory
layout. The library allocates a private mapping for the reconstructed image,
which remains available until the library unloads. Its ABI version stays 5.

To compare with a flat bundle, run `bin2cbundle.py -t vmlinux` without
`--compact`. The compiled ABI roundtrip tests require `cc` and `pyelftools`:

```sh
just fw test
```

### Linux (SEV variant)

#### Requirements
* The toolchain your distribution needs to build a Linux kernel.
* Python 3
* ```pyelftools``` (package ```python3-pyelftools``` in Fedora and Ubuntu)

#### Building and installing the library
```
make SEV=1
sudo make SEV=1 install
```

### macOS

#### Building the library using krunvm

Compiling a Linux kernel natively on macOS is not an easy feat. For this reason, the recommended way for building ```libkrunfw``` in this platform is by already having installed a binary version of [krunvm](https://github.com/containers/krunvm) and its dependencies ([libkrun](https://github.com/containers/libkrun), and ```libkrunfw``` itself), such as the one available in the [krunvm Homebrew repo](https://github.com/slp/homebrew-krun), and then executing the [build_on_krunvm.sh](build_on_krunvm.sh) script found in this repository.

This will create a lightweight Linux VM using ```krunvm``` with the current working directory mapped inside it, and build the kernel on it.

```
./build_on_krunvm.sh
make
```

By default, the build environment is based on a Fedora image. There is also a Debian variant which can be selected by setting the `BUILDER` environment variable.

```
BUILDER=debian ./build_on_krunvm.sh
```

In general, `./build_on_krunvm.sh` will always delegate to `./build_on_krunvm_${BUILDER}.sh` so additional environments can be added like this if needed.

#### Building the library natively (experimental)

The Linux kernel can be compiled natively on macOS. You will need some dependencies:
* LLVM toolchain (`clang`, `lld`, etc.) (the version provided by Apple may not work)
* GNU `make`
* GNU `sed`
* GNU `tar`
* A case-sensitive file system

The name of the `make`, `cc`, and some other executables can be overridden as arguments to the Makefile. If needed, you can also prepend to the `PATH` so that GNU versions of executables are chosen at higher priority.

```
# Create a case-sensitive disk image and mount it
hdiutil create -size 8g -type SPARSE -fs "Case-sensitive APFS" -volname libkrunfw-cs ../libkrunfw-cs.sparseimage
hdiutil attach ../libkrunfw-cs.sparseimage

# Copy this source tree into the mounted disk image
rsync -a ./ /Volumes/libkrunfw-cs/

# Set up paths to the dependencies as needed, for example
export PATH=/path/to/dir/containing/gnu/sed:/path/to/dir/containing/gnu/tar:$PATH
make MACOS_BUILDER=native
```

### Windows (cross-compilation from Linux)

#### Requirements
* A Linux host with the toolchain needed to build a Linux kernel.
* The MinGW-w64 cross-compiler (`x86_64-w64-mingw32-gcc`), available as `mingw-w64-gcc` (Fedora) or `gcc-mingw-w64-x86-64` (Debian/Ubuntu).
* Python 3
* ```pyelftools``` (package ```python3-pyelftools``` in Fedora and Ubuntu)

#### How it works

The Windows build uses a dedicated kernel configuration (`config-libkrunfw-windows_x86_64`) that enables Hyper-V guest enlightenments (`CONFIG_HYPERV`, `CONFIG_HYPERV_TIMER`, `CONFIG_HYPERV_UTILS`), allowing the guest kernel to take advantage of the Windows Hypervisor Platform (WHP).

The kernel bundle is aligned to 4K pages (instead of 64K on Linux) to match the page granularity used by x86_64 Windows and WHP, avoiding alignment mismatches when the VMM maps the kernel into guest memory.

The resulting `libkrunfw.dll` is produced using the MinGW-w64 toolchain and can be consumed by the Windows build of [libkrun](https://github.com/containers/libkrun).

#### Building the library
```
make OS=Windows
```

This will:
1. Download and patch the kernel sources.
2. Build the kernel using the Windows-specific configuration.
3. Generate the C bundle with 4K page alignment.
4. Cross-compile `libkrunfw.dll` using `x86_64-w64-mingw32-gcc`.

## Known limitations

* To save memory, the embedded kernel is configured with a limited number of CPUS. The CPU limit depends on the config target. If this kernel runs in a VM with more CPUs than it is configured for, only the first N CPUs will be initialized and used.

| Target         | `NR_CPUS=` |
|----------------|------------|
| x86_64         | 16         |
| sev_x86_64     | 8          |
| tdx_x86_64     | 8          |
| windows_x86_64 | 16         |
| aarch64        | 16         |
| riscv64        | 16         |

## License

This library bundles a Linux kernel but does not execute any code from it, acting as a mere storage format. As a consequence, this library does not constitute a derivative work of the Linux kernel. Thus, the following licenses apply:

* **Linux kernel**: GPL-2.0-only

* **Files contained in the ```patches``` directory**: GPL-2.0-only

* **Library code, including automatically-generated code**: LGPL-2.1-only

Therefore, distributions of this library in binary form are required to be accompanied by the source code of the Linux kernel bundled in the binary along with the code of the library itself, but other programs linking against this library are not required to be licensed under the GPL-2.0-only nor the LGPL-2.1-only licenses.
