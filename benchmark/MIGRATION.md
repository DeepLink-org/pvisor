# Criterion migration inventory

This is an engineering workflow inventory, not a new performance publication.
The source of entry identity/readiness remains `registry.csv`. Historical
measurements, raw data, public tables and approval records are not changed.

## Thirty implemented entries

All entries below route to the standalone Rust crate pinned to Criterion 0.7.0.
“Contracts” means compilation and bounded fixture/validator/lifecycle tests,
**not** a qualified real-machine experiment. Criterion calibration and measured
operations share the same correctness checks. Platform prerequisites are not
silently skipped or reported as zero samples.

| Registry entry | Rust family | Workflow validation and remaining environment |
| --- | --- | --- |
| `memory/allocator_reclaim.json` | `src/memory/` | Contracts; real Apple Silicon VM/process accounting unrun |
| `storage/lazy_local_reads.json` | `src/storage/` | Contracts; authenticated images/private Docker/V2 full experiment unrun |
| `storage/lazy_storage.json` | `src/storage/` | Contracts; real four-image inventory unrun |
| `storage/lazy_runtime.json` | `src/storage/` | Contracts; real prepared runtime/memory cohort unrun |
| `storage/lazy_remote.json` | `src/storage/` | Contracts including registry/proxy byte checks; full cold/warm image experiment unrun |
| `startup/image_compare.json` | `src/startup/images/` | Contracts; real private Docker/OverlayBD/KVM lab unrun |
| `startup/image_lazy_compare.json` | `src/startup/images/` | Contracts; real cold/warm lab unrun |
| `startup/stage_lazy_compare.json` | `src/startup/images/` | Contracts; real FUSE/OverlayBD/firmware lab unrun |
| `filesystem/tools.md` | `src/filesystem/` | Real Rust prepare/metadata/read/validate fixture smoke; contracts |
| `filesystem/user_workloads.json` | `src/filesystem/` | Fresh fixture/USTAR/PID1/payload contracts; nine-runtime campaign unrun |
| `filesystem/dax_ab.json` | `src/filesystem/` | Driver compiled against product API; guest contracts; real DAX VM unrun |
| `network/profile_ab.json` | `src/network/` | Worker/protocol/ownership contracts; real VM network A/B unrun |
| `replay/replay.md` | `src/replay/` | Seven real CLI prepare-only adapter smokes and native-fixture goldens; no formal campaign |
| `memory/scanner_compare.json` | `src/memory/` | SDK report/placement/lifecycle contracts; real scanner VM cohort unrun |
| `memory/pool_compare.json` | `src/memory/` | Native fixture/probe/preparation contracts; real memory/cgroup cohort unrun |
| `memory/library_compare.json` | `src/memory/` | Native dlopen/oracle/preparation contracts; real DAX/PTE/VM cohort unrun |
| `startup/pvisor_vm_timing.json` | `src/startup/` | Ready/Exit contracts; real firmware/KVM backend unrun |
| `process/smoke.md` | `src/process.rs` | Actual four-condition Criterion workflow smoke using debug product; not release performance evidence |
| `process/nightly.md` | `src/process.rs` | Shared implementation/contracts; larger formal nightly run intentionally unrun |
| `startup/native_timing.json` | `src/startup/` | Fresh native subject/Ready/Exit fixture smoke and contracts |
| `startup/pvisor_host_timing.json` | `src/startup/` | Contracts; full startup-specific backend experiment unrun |
| `startup/pvisor_staged_timing.json` | `src/startup/` | Contracts; real staged backend experiment unrun |
| `startup/docker_timing.json` | `src/startup/` | Contracts; prepared private Docker backend unrun |
| `startup/fc_system_timing.json` | `src/startup/` | Stock-kernel/receipt/output contracts; real Firecracker experiment unrun |
| `startup/qemu_timing.json` | `src/startup/` | Stock-kernel/receipt/output contracts; real QEMU experiment unrun |
| `startup/qemu_microvm_timing.json` | `src/startup/` | Receipt/output contracts; real QEMU microvm experiment unrun |
| `startup/timing.json` | `src/startup/` | Routes all eight real implementations; eight-backend campaign unrun |
| `storage/image_storage.json` | `src/storage/` | Contracts; real private inventory unrun |
| `storage/image_format.json` | `src/storage/` | Metric/config/lifecycle contracts; real paired image reader unrun |
| `storage/native_runtime.json` | `src/storage/` | Contracts; real native Docker/V2 runtime/memory cohort unrun |

Native Rust memory preparation is `pvisor-memory-prepare` (`pool`, `scanner`,
`allocator`, `docker`); these commands prepare fixtures/receipts but never sample.
Product-linked probes are behind `vm-probes`, so default tool tests do not build
unrelated product crates. Linux and macOS capabilities are explicit in family
READMEs; Linux tests do not qualify Apple Silicon or stock-kernel/DAX behavior.

## Thirty-three intentionally unprepared entries

These were unprepared before the migration and remain rejected before execution.
No arbitrary workload or old summary has been substituted.

| Benchmark ID | Registry entry |
| --- | --- |
| B-OPENSANDBOX-ENG | `startup/opensandbox.json` |
| B-SANDBOX-ENGINE-ENG | `startup/sandbox_engine.json` |
| B-OPENSANDBOX-BURST-DIAG | `startup/opensandbox_burst.json` |
| B-OPENSANDBOX-MEMORY-ENG | `memory/opensandbox_memory.json` |
| B-LAZY-STARTUP | `startup/lazy_startup.json` |
| B-STARTUP-ENG | `startup/guest_init.json` |
| B-KERNEL-ENG | `startup/kernel.json` |
| B-STARTUP-DIAG | `startup/diagnostic.json` |
| B-FS-ENG | `filesystem/ab.json` |
| B-FS-DIAG | `filesystem/diagnostic.json` |
| B-LAZY-ENG | `filesystem/lazy_image.json` |
| B-APPLY | `workflow/apply.json` |
| B-APPLY-ENG | `workflow/apply_ab.json` |
| B-APPLY-DIAG | `workflow/apply_diagnostic.json` |
| B-SUPERVISION | `workflow/review.json` |
| B-WORKFLOW | `workflow/workflow.json` |
| B-NETWORK | `network/network.json` |
| B-PEER-NET-ENG | `network/peer.json` |
| B-FOUR-PEER-ENG | `network/four_peer.json` |
| B-PEER-CHAOS-ENG | `network/peer_chaos.json` |
| B-DENSITY | `density/density.json` |
| B-ISOLATION | `isolation/isolation.json` |
| B-AGENT-TASK | `agent/agent.json` |
| B-VM-MEMORY | `memory/memory.json` |
| B-MEMORY-SCALE | `memory/scale.json` |
| B-MEMORY-DIAG | `memory/diagnostic.json` |
| B-COLD-RUNTIME-ENG | `memory/cold_runtime.json` |
| B-VCPU-IDLE-ENG | `memory/vcpu_idle.json` |
| B-COLD-STORAGE-DIAG | `memory/cold_storage.json` |
| B-MACOS | `agent/macos.json` |
| B-MACOS-ENG | `agent/macos_ab.json` |
| B-STARTUP-REVISION-ENG | `startup/revision.json` |
| B-SWE-RESIDENT-ENG | `agent/swe_resident.json` |

## Statistical and publication boundary

Criterion's adaptive observations are a new cohort. Old fixed N3/N30/N60 schedules
are not claimed as an exact operation-count match. Complete randomized pairs or
matrices remain raw evidence, while selected genuine clocks/units are submitted
to Criterion. Callback ledgers do not guess which invocations were “formal.”

No handwritten replacement sampler/bootstrap pipeline is installed. Criterion
independent-arm intervals do not satisfy paired treatment-effect, multimodality
or tail-latency publication gates. The migration does not publish those claims.
New public bindings and qualifying evidence are still required by `REGISTRY.md`.

Documentation tables are ordinary Markdown; their exact rows are checked
against retained JSON/CSV. Display/archive helpers are separate from measurement.
Product cases use Scrut with preservation and resource fixture checks.
