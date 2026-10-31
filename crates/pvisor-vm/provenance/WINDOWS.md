# Windows backport

The reference application is
[microsandbox](https://github.com/superradcompany/microsandbox/tree/c18f7d5880f01a3118cc8ce2ee02b2c651681bf3).
Its native Windows runtime is maintained in
[superradcompany/libkrun](https://github.com/superradcompany/libkrun/tree/krun),
distributed as `msb_krun`. pVisor imports source into its existing VM crate;
there is no runtime dependency on that SDK or its C API.

The private `src/windows/{arch,devices,kernel,polly,utils,vmm}` source baseline is
commit [`7e90097bb568b0c9ff7ca3be264744a2dca5a740`](https://github.com/superradcompany/libkrun/commit/7e90097bb568b0c9ff7ca3be264744a2dca5a740)
(0.1.22). This includes the initial WHP port (#71), aligned WHP register buffers
(#73), application-processor startup (#79), CPUID/PIT changes (#80) and TSC CPUID
leaf 0x15 (#82). The later microsandbox SDK contains unrelated API, nested
virtualization and snapshot changes; this backport does not import those wholesale.

Also adapted:
[`ff7f4b5254c8984b175169b9ef5ee02b51ff2fbf`](https://github.com/superradcompany/libkrun/commit/ff7f4b5254c8984b175169b9ef5ee02b51ff2fbf)
(#156), passing the WHP processor clock frequency as `tsc_early_khz` before caller
arguments and the init separator. This is needed for AMD guests, which do not
use Intel's CPUID-based TSC calibration. The argument-ordering tests are retained.

Local integration:

- Platform modules are private and compile only on Windows. The Unix runtime
  and its existing snapshot/Overlay integrations retain their implementation.
  Imports resolve inside `pvisor-vm`; `vm-memory` remains pinned to 0.17.1 with
  its portable mmap backend on Windows, and the Windows bindings are 0.61.2.
- Public contracts remain unconditional in `api.rs`; the native stream wrapper
  removes `UnixStream` from signatures while preserving Unix caller conversion.
  `LayerMutability` moves to its portable record owner, `pvisor-core`, with the
  existing overlay-core re-export preserved. This avoids linking Unix overlay
  services merely to name an API record on Windows.
- The first-party Windows builder owns firmware and attachments. Filesystems
  use current pVisor supervisor bytes, root-only authenticated PID 1 exit and
  owned synthetic files. Legacy unauthenticated exit and root deletion requests
  are denied. Read-only shares enforce writes in the host backend too.
- Ready callbacks execute before vCPU resume. Control transitions acknowledge
  every vCPU and latch failures. Teardown cancels/joins vCPUs and PIT/device
  workers before destroying WHP resources. The upstream detached AP debug
  probe is removed because it could outlive its partition.
- Advanced profiles without a completed Windows implementation return explicit
  unsupported errors. Native contract/unit tests run independently of WHP;
  the separate hardware smoke job boots the repository's Windows firmware.

Upstream copyright/SPDX headers are retained. The imported code is Apache-2.0
with the notices in [WINDOWS-THIRD-PARTY](WINDOWS-THIRD-PARTY). The guest kernel
and firmware licensing remains documented separately in `fw/README.md`.

Validation boundaries: cross-compilation is not execution; Win32 unit tests do
not prove WHP availability; passing an x86_64 WHP smoke does not establish
Windows ARM64, nested virtualization, snapshots, Overlay or full product support.
