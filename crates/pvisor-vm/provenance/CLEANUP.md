# Optional-device cleanup audit

## Product-call and build evidence

The workspace dependency declarations select `pvisor-vm` as follows:

- `crates/pvisor/Cargo.toml`: `net`.
- `crates/pvisor-cli/Cargo.toml`: Unix selects `net`; Windows uses the defaults.
- `crates/pvisor-shim/Cargo.toml`: the opt-in `vm` profile selects `net`.
- The root workspace dependency adds no optional VM device profile.

No current first-party caller selects `gpu`, `snd`, `input`, `blk`, `tee`,
`amd-sev`, `tdx`, `aws-nitro` or `efi`. The public `src/api.rs` has no GPU,
sound, input, block-device or attestation configuration contract.
`.github/workflows/ci.yml` exercises normal product builds; Windows CI explicitly
builds/tests the default and `--no-default-features` VM configurations and the
`whp_smoke` example. Neither CI nor `scripts/` or `mise.toml` selects the retired
device features. The build script's EFI handling is still present and is part
of the deferred EFI profile, not evidence of a product caller.

Removed in this step:

- `gpu`, `snd`, `input`, and the GPU-only `virgl_resource_map2` feature.
- The standalone Unix GPU/sound/input implementations, private resource fields,
  constructors, attachment dispatch, error variants and GPU SHM allocation.
- Equivalent unreachable optional dispatch/resource declarations in the Windows
  backend, not the WHP backend or the Windows implementation tree.
- Direct optional dependencies `krun-display`, `krun-input`, `pipewire`,
  `krun-rutabaga-gfx`, `thiserror` and `zerocopy`.

There is no replacement public no-op: Cargo rejects a removed feature selection.
The API remains uniform and unchanged. Existing explicit `Unsupported` errors
for unavailable public operations remain in place.

Retained for a separate bounded change: `blk`, `tee`, `amd-sev`, `tdx`,
`aws-nitro` and `efi`. These have no product-call evidence either, but their
interdependent payload loading, architecture layout, worker messages and vCPU
branches extend through the core VMM; they were not mechanically pruned in this
step. Their presence is not a supported-product-profile claim.
`serde`, `test_utils`, `net` and `init-blob` are retained.
All RAM/FS experiment environment switches are unchanged.

The shared-memory mapping messages still named `GpuAddMapping` and
`GpuRemoveMapping` are also used by virtio-fs passthrough. They and their
platform mapping implementations are deliberately retained. Their names alone
are not evidence that they belong exclusively to the removed GPU device.
The `ExportTable` filesystem plumbing is likewise retained with no GPU producer.

Historical manifests, packaged VCS records, upstream/backport ledgers,
third-party notices and all surviving source copyright/SPDX headers remain.
This is a selective deletion from the recorded libkrun baseline, not a new
upstream version or a VMM/API rewrite.

## Dead-code diagnostic experiment

On the Apple Silicon host, temporarily removing only `dead_code` from the ten
existing `runtime_modules.rs` module-level allow attributes produced:

| Check | Result |
| --- | --- |
| `mise exec -- cargo check --locked -p pvisor-vm --features net`, before device removal | Successful; 160 VM warnings |
| `mise exec -- cargo check --offline -p pvisor-vm --features net`, after device removal | Successful; 165 VM warnings |
| `mise exec -- cargo check --offline -p pvisor-vm --no-default-features --target x86_64-pc-windows-msvc`, after device removal | Successful cross-check; 991 VM warnings, including imported Windows naming warnings |

These counts are compiler diagnostics, not counts of individual dead symbols.
Unused-import allowances were not removed. Full compiler transcripts were
captured as validation artifacts, not committed as machine-specific source logs.
The original module allowances were restored after this experiment: no new or
broader allowance was added. This does **not** establish a zero-dead-code gate.

Representative remaining diagnostic groups:

- Platform-specific architecture/register/interrupt definitions:
  `src/arch/aarch64/`, `src/arch_gen/`, `src/hvf/`. Their non-use on this target
  does not establish non-use on Linux or in saved-machine contracts.
- Shared device interfaces, virtqueue helpers and wire definitions:
  `src/devices/virtio/{device,descriptor_utils,file_traits,mmio}.rs`.
  Retained block/TEE paths and tests consume some of these; no shared interface
  was removed solely from a macOS default-profile warning.
- Inherited error/configuration/helper surfaces:
  `src/vmm/{mod,builder,resources}.rs`,
  `src/vmm/vmm_config/{external_kernel,kernel_bundle,machine_config}.rs`,
  `src/kernel/cmdline/mod.rs` and `src/polly/event_manager.rs`.
  Examples include unused error variants, `build_microvm`,
  `create_guest_memory`, `InstanceInfo`, external kernel formats and CPU
  templates. Removing them requires a separate caller/platform/test audit.
- Worker and mapping interfaces:
  `src/utils/worker_message.rs`, `src/vmm/worker.rs`,
  `src/vmm/macos/vstate.rs`. Removing the GPU-only worker startup exposes
  additional warnings, but the mapping messages still have filesystem senders.
  A complete filesystem/worker ownership review is deferred rather than
  changing DAX behavior here.
- Windows cross-check warnings include inherited naming conventions plus
  unused WHP mapping helpers, vsock configuration, API opaque-state fields
  and the Unix startup profiler. They are not macOS evidence and do not imply
  that the Windows runtime is unused.

Next work should audit and narrow one module at a time, compile the affected
platform/profile and retain external API contract coverage. Do not reinterpret
this experiment or restored allowances as proof that all unused code is needed.

## Validation boundaries

The first `mise run test -- pvisor-vm` ran 421 tests: 420 passed, one failed,
three skipped. Public API and repository-boundary tests passed.
`contract_tests::vm_snapshot_vsock::listeners_reject_snapshot_and_failed_save_does_not_rebind_them`
timed out at the existing five-second polling deadline; an isolated retry with
the same test command and test-name filter also timed out. No timeout, assertion
or vsock implementation was changed to hide the failure.

After adding the retired-device guard and correcting its empty-directory check,
`mise run test -- pvisor-vm -- --test api_boundary --test api_contract --test repository_boundary`
passed all 17 tests. `mise exec -- cargo fmt --check -p pvisor-vm` and
`git diff --check -- crates/pvisor-vm` passed.
Final `mise exec -- cargo check --locked -p pvisor-vm --features net`,
`mise exec -- cargo check --locked -p pvisor-shim --features vm`, and
`mise exec -- cargo check --locked -p pvisor-cli --bin pvisor` passed; the last
also compiled the `pvisor` consumer. The concurrently edited overlay-core emitted
two unused-variable warnings outside this cleanup.

Warnings-denied rustdoc passed. The project's pinned `mise` toolchain could
cross-check Windows without `init-blob`, but its Linux x86-64 musl standard
library was missing; that also prevented the default Windows guest build.
An explicitly selected, already-installed `+stable` toolchain with the same
`rustc 1.98.0` version successfully checked the default Windows profile.
The supplemental `+stable` Linux musl check then stopped at the missing
`x86_64-linux-musl-gcc` compiler in `zstd-sys`, before checking the VM itself.
No toolchain configuration or target installation was changed.

### Final bounded-concurrency validation of the first cleanup

The parent thread subsequently completed:

```sh
TMPDIR=/tmp mise run test -- pvisor-vm pvisor-gateway pvisor-core pvisor-daemon -- --test-threads 4 --no-fail-fast
```

Result: **1059 passed, 8 skipped**. This supersedes the earlier incomplete
full-run result, not its failure history. The same vsock test passed with the
short temporary root; macOS's default long temporary root had caused the socket
path failure. An earlier high-concurrency run also encountered `VmCreate`
failure; its isolated rerun passed. No vsock deadline or assertion was weakened.
The final result establishes the stated short-root, four-thread run, not an
unrestricted default-environment or high-concurrency pass.

Before that final run, twelve daemon image tests exposed fixtures hard-coded to
amd64; three fault tests then exposed insufficient macOS XDG isolation. The
parent thread changed the fixtures to use the native platform and isolated HOME
alongside the XDG paths. Assertions were not weakened. These fixture corrections
are outside the VM device cleanup.

Cross-checking does not execute WHP or KVM. No real-guest HVF/KVM/WHP boot,
Linux execution test or native Windows execution test was performed in this
cleanup. The passing bounded-concurrency run does not fill those platform gaps.

## Next-stage review: legacy block and protected-boot profiles

The next review made no source or manifest changes. `blk`, `tee`, `amd-sev`,
`tdx`, `aws-nitro` and `efi` remain intact; no additional deletion or validation
result is claimed. `amd-sev`, `tdx` and `efi` directly select `blk`, so retiring
block alone must not leave profiles with silently reduced device inputs. The
agreed deletion unit, if resumed, is this interdependent legacy group. It is
deferred rather than partially pruned; the next planned cleanup instead reviews
platform-independent duplicate Windows modules for sharing.

Its conditions span payload selection, protected VM setup, measured launch,
guest-memory layout, workers and vCPU startup in both backend trees. Removing
the EFI feature must retain explicit caller-selected firmware: the ordinary
`firmware_config` path also selects the firmware payload and reads its bytes.
Windows block-specific volatile I/O, read-plan tests and descriptor helper
conditions require their own review; ordinary file I/O and retained filesystem
consumers must survive. No generic cfg-pruning framework or new lint allowance
was introduced.
