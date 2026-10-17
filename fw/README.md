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

The virtio-fs directory image preserves zero-validity lookup-only entries for
one-shot injected files. Even after a COMPLETE import, these names use host
LOOKUP and stay hidden from readdir. READINDEX rejects replies beyond its page
budget and incomplete replies without forward progress.

Deferred regular-file opens retain their original credentials and bind before
unlink, rename, copy-up, or attribute changes so existing guest descriptors keep the
original host object. Binding and release share an inode-local lock. Simple
seeks stay local, and unbound attribute refreshes do not send a fictitious handle.
Directory images hold no child dentry references; ordinary inode eviction frees
the image. Hosts decline READINDEX for mutable or unspecified backings, retaining
ordinary FUSE namespace visibility and metadata timeouts.

`patches/0046-fuse-negotiate-local-readonly-flush.patch` adds the opt-in
`FUSE_PVISOR_LOCAL_FLUSH` capability (bit 43) and per-open
`FOPEN_PVISOR_LOCAL_FLUSH` promise (bit 29). The host only grants the latter
after its normal OPEN checks for a read-only immutable local lower whose
backend FLUSH has no work or errors to report. Host policy remains default-off;
neither the guest capability nor a read-only mount establishes that ownership
or backing-filesystem promise. Old peers keep the existing FLUSH path.

The guest rejects this capability with WRITEBACK_CACHE or remote POSIX/flock
locks, and additionally checks regular-file, read-only and non-DAX status at
flush time. It retains `write_inode_now`, `fuse_sync_writes` and
`filemap_check_errors` and their error returns; only then may it skip the
backend FLUSH request through the ordinary attribute-exit path. It does not use
`FOPEN_NOFLUSH`'s early return, suppress explicit fsync, change KEEP_CACHE/TTL,
or alter RELEASE and submount lifetime rules. `fw/tests/test_local_flush_contract.py`
compiles the actual patched flush function against observable call stubs to
check local error ordering and ordinary-RPC fallback; real guest protocol,
backing-close errors and teardown still require independent integration runs.

`patches/0047-fuse-isolate-immutable-lower-data.patch` adds the separate,
default-off `FUSE_PVISOR_DATA_OBJECT` capability (bit 44). With this capability,
every `FUSE_OPEN` reply is exactly 24 bytes: the unchanged 16-byte
`fuse_open_out`, followed by a 64-bit fixed lower size. `FOPEN_PVISOR_DATA_OBJECT`
(bit 28) makes `backing_id` a nonzero, connection-local immutable data-object
identifier; it is not a guest passthrough FD. Unmarked replies have no data
promise. OPENDIR, CREATE and peers without this capability retain their ordinary
reply layouts. The host must keep object identifiers stable and never reuse
them for different lower bytes while the guest connection remains alive.

Marked read-only regular opens keep the existing namespace file, host fh and
nodeid. They read through a separate, tagged FUSE data inode and a real
`FMODE_BACKING` file. Equal object IDs on the same superblock share data pages;
copy-up, upper writes and namespace size changes use the original mapping.
Hidden inodes own no LOOKUP references and send no FORGET. They use ordinary
inode reclamation so cached pages may survive close without retaining closed
host handles. The fixed EOF comes from OPEN, never merged attributes. Local
allocation failure retries only the local mapping setup, not the successful
native OPEN, and never silently falls back to the merged mapping.

Read/pread, splice and mmap use the kernel backing-file helpers. fadvise and
readahead(2) target the inner mapping too, including explicit WILLNEED. The hidden
file exposes only read operations and a read-only shared mmap; MAP_PRIVATE COW
remains available. A VMA and asynchronous readahead retain the inner file,
mapping, namespace path, mount and native handle until completion; only the
last reference sends the original RELEASE, preserving auto-submount synchronous
release. Real host READ completion invalidates namespace ATIME; a page-cache
hit does not add artificial ATIME invalidation. A successful short READ before
fixed EOF fails the affected page or readahead batch with EIO; it cannot shrink
the receipt EOF or publish zero-filled data as Uptodate. A real partial EOF tail
remains valid. Changing O_DIRECT after open still directs reads through the original host handle, using the hidden inode's
fixed EOF; it does not silently use cached pages or reject F_SETFL. Hidden file
opens require FMODE_BACKING, so a procfs magic link cannot authorize a new data
handle. SEEK_END uses the fixed lower
size while updating the outer file position, and DATA/HOLE retain the original
host fh. Normal OPEN authorization and observation remain host operations.

This experimental capability requires WB=0, DAX=0 and no remote file locks;
DIRECT_IO, passthrough, invalid IDs and sizes beyond the filesystem limit are
rejected for marked replies. The host must reject capture/restore while it is
negotiated: the hidden guest pages and object IDs are not a snapshot schema.
This change does not redefine fstat/O_PATH or other namespace metadata identity.
In particular, VFS copy_file_range checks still inspect the outer namespace
inode's size before the FUSE method runs; the capability does not claim to fix
that pre-existing old-fd metadata/EOF boundary. Real guest page ownership,
partial/truncate, mmap/exec, cross-open reuse and release behavior need separate
validation before any performance or default-enablement claim.

`patches/0045-fuse-preserve-open-and-lookup-lifetimes.patch` shares native
READDIRPLUS parallel dentry allocation and directory alias splicing with directory
imports. Imports run outside the ordinary lookup mutex so competing host lookups
can finish. Deferred-open registration waits only while a mutation is active;
the existing inode-local mutex still serves the uncontended path. Writable OPEN,
unlink, rename, hard-link creation, every SETATTR shape and xattr mutations bind
earlier deferred regular-file descriptors and exclude new registrations until
the host request replies. This gate covers regular-file endpoints; descendant
descriptors during a directory rename that materializes a subtree still need
separate coverage. Binding precedes FUSE_NOWRITE to avoid laundering pages
while writeback is blocked. Queue-held final file references preserve virtio-fs
synchronous RELEASE for auto-submounts; deferred-open poll failures return EPOLLERR.

`patches/0044-fuse-adaptive-directory-prefetch.patch` combines the directory
image safeguards and adaptive prefetch policy. READINDEX ENOSYS is cached per
connection without disabling deferred open. Lookups concurrent with import
fall back to the host, leaving in-flight lookup references owned by the importer.
It admits one page after
demand for different name hashes. Admission uses directory size as a hint,
requiring 4 to 64 distinct misses; size is not treated as a trusted entry count.
The initial window is four guest pages;
it expands to the negotiated limit only if it contained the demanded name
and the use count reaches three quarters of the imported entries before the
next miss. Existing demanded dentries count immediately; other inodes count
their first later hit. This avoids reopening a wide host directory per tiny page.
An inode flag counts its first metadata hit even during RCU pathname walks;
only inode state is touched, so no cache-image lifetime is extended.
A miss outside the fetched window disables speculative lookup prefetch until
inode eviction; explicit readdir can still complete the image. Repeated access
to one name does not advance the admission hint. The triggering lookup owns its
imported reference, while concurrent lookups retain the host fallback. Guest
namespace changes invalidate partial images and fall back to the host, because
they change the offsets and cookies of the unfinished directory snapshot.
The triggering lookup validates inode IDs and attributes before consuming its
imported reference, matching the ordinary lookup path.

With negotiated `FUSE_PVISOR_WRITEBACK` (bit 42) and WRITEBACK_CACHE, ordinary
cached writes and closes retain inode/handle leases in a connection-local queue.
The queue uses ordinary FUSE WRITE requests and a bounded 10 ms submission
timer. `0049-fuse-coalesce-writeback-kicks.patch` advances that timer when dirty
pages reach one quarter of the connection's pressure target, clamped to
1–16 MiB. A 512 MiB guest therefore normally has a 16 MiB submission hint;
this is separate from its approximately 64 MiB pressure target and does not
change the negotiated 1 MiB maximum WRITE payload. Small and low-rate writes
still progress on the timer; the 256-handle limit and explicit synchronization
can force earlier progress.

Queue admission reads only an approximate dirty-page counter, before taking
the connection lock. This count controls early scheduling, so its bounded
error cannot weaken the actual per-folio pressure check. It does not repeat
that check after a buffered write returns, sum all per-CPU counters per small
write, or schedule an empty submit pass for a handle already being completed.
Completion retains responsibility for requeueing a redirtied handle.
For virtio-fs, 0049 reserves scatterlist scratch before starting each request
virtqueue and reuses it under the existing queue lock through descriptor
submission. A full 1 MiB WRITE no longer allocates a transient 16 KiB
scatterlist with `GFP_ATOMIC`, which cannot reclaim clean cache when free
memory is low. The scratch capacity covers both negotiated page vectors and
their headers; its lifetime belongs to the existing device owner, including
partial setup failure. Request argument buffers and data pages retain their
original per-request completion lifetime. Virtqueue descriptor admission and
its existing backpressure stay unchanged.
`0048-fuse-pipeline-native-writeback-pressure.patch` separates
best-effort `WB_SYNC_NONE` submission from a data-integrity completion worker;
a completion wait on one inode does not stop later files from being submitted.
Completion waits for that inode's already submitted temporary WRITEs before
the native data/metadata integrity pass, which may send timestamp SETATTR.
It retains a second WRITE barrier after that pass for newly submitted pages.
This avoids holding a host namespace-admission lock behind an earlier WRITE
when the host orders SETATTR against the same inode's active I/O. Host namespace
serialization and guest metadata RPCs remain separate possible bottlenecks.
The queue retains at most 256 inode/handle leases across both stages, through
backend FLUSH, RELEASE and final reference disposal. Virtio-fs auto-submount
RELEASE remains synchronous. Read-only descriptors retain ordinary native FLUSH
and its errors; writable close may report a previously recorded asynchronous
error while its final FLUSH/RELEASE completes later.

Producer pressure uses the native BDI's actual dirty pages plus FUSE's temporary
WRITE pages. Repeated writes to an already dirty page and multiple descriptors
do not accumulate fictitious byte credits. Each connection/BDI has a target of
one eighth of managed guest RAM, clamped to 16–256 MiB; a 512 MiB guest has
approximately a 64 MiB target per BDI. Independent rootfs/workspace BDIs each
have a target and handle bound; the native global dirty limit still constrains
their combined pressure. Buffered writes check pressure after unlocking each completed folio,
including within a single large write, and wake the native BDI flusher. Native
WRITE completion can release this pressure independently of the completion
worker's inode lock. A timed recheck also observes native flusher and mmap
writeback progress. `/sys/module/fuse/parameters/pvisor_wb_budget_bytes` reports
the active target after negotiation and rejects writes; it is not a tuning
parameter. Native per-CPU counters provide a cheap check away from the target;
near it, their full sums decide whether to wait. Native dirty throttling and
memory reclaim remain active, including for mmap. Concurrent dirtying can
overshoot the target; it is not an allocation cap on all guest pagecache, and
clean cached pages remain outside this pressure budget. Host background
requests are independently bounded. Pages remain in the native guest cache.
Negotiated sessions lift FUSE's default 1% dirty-share ceiling and request a
16 MiB minimum dirty share, avoiding cold-device bandwidth throttling before
the queue reaches its pressure target. Strict FUSE accounting stays enabled,
including for temporary writeback pages. The reservation is best effort when
other mounts or a small guest exhaust the global dirty-share budget; ordinary
Linux memory-pressure limits still apply, and reservation failure is logged.
The BDI ratio setters are not a persistent fixed-byte limit.
fsync and O_SYNC/O_DSYNC keep their synchronous native paths; syncfs/unmount
drain both workers, and asynchronous
errors stay visible to later synchronization. Remote-lock and DAX sessions do
not use this extension. Original lower reads after copy-up bypass the upper
inode's data cache through their retained handles. The guest supervisor must
check final virtio-fs synchronization before reporting successful task exit.
`fw/tests/test_writeback_pipeline_contract.py` compiles the actual patched
functions with observable stubs to check page-accounting decisions, long-write
lock ordering, separate submission, rescan/drain, sticky errors and final
reference/RELEASE ordering. The independent
`fw/tests/test_writeback_kick_contract.py` executes 0049's actual queue body
alongside those unchanged lifecycle and pressure functions, checking the
submission hint, non-extending timer, cheap queue admission and rescan after
an in-flight generation. `fw/tests/test_virtio_sg_scratch_contract.py` executes
the actual allocation, final cleanup and enqueue bodies with observable
allocation, queue-lock and descriptor-copy stubs. It checks partial setup
failure, bounded scratch reuse and retained per-request argument buffers;
it does not replace real memory-pressure or concurrent transport validation.
Firmware compilation and real guest stress,
concurrent mmap/reclaim, failure/disconnect, synchronization and teardown tests
remain required before making performance or default-enablement claims.

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
