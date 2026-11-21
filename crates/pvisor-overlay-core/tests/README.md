# Overlay Core contract tests

Run these tests through `mise run test -- pvisor-overlay-core`. They use public
exports and private temporary backing, without FUSE mounts or VMs.

## Ownership

| Contract | Entry point | Oracle |
| --- | --- | --- |
| Namespace operations, errors and reopen | `namespace_recovery.rs` | Disposable native filesystem plus first-touch fingerprints |
| Selective apply, conflicts, recovery and drop | `selective_lifecycle.rs` | Independent expected tree, candidate set and durable ledger |
| Content receipt encoding | `content_receipts.rs` | Input inventory and encoding invariants |
| Checked lookup and policy | `checked_backing_lookup.rs`, `protected_trees.rs`, `read_conflicts.rs` | Returned errors, unchanged backing and retained observations |
| Copy-up ownership | `prepared_copy_up.rs`, `direct_copy.rs`, `copied_hard_link_metadata.rs` | Bytes, inode sharing, cancellation and source validation |
| Backing and caching | `private_backing.rs`, `lower_immutability.rs`, `stable_namespace.rs` | Logical/native metadata, explicit leases and fresh observations |
| Attachment routing | `backend_routing.rs` | Explicit root/relative-path bytes, generated paths, rejection without dispatch and bounded in-flight ownership handoff |
| Stage completion | `seal_publication.rs`, `unrecorded_stage.rs`, `preimage_file_types.rs` | Lifecycle admission, publication failure and bounded process checks |
| Recording admission | `recording_admission.rs` | Constructor/mode equivalence, exact policy bytes, first baselines and unchanged metadata on rejection |

`namespace_recovery.rs` owns traces and generators; `namespace_recovery/model.rs`
owns operations, native comparison and journal assertions. The `scenario` module
in `selective_lifecycle.rs` owns lifecycle transitions, while
`selective_lifecycle/oracle.rs` owns snapshots and expected-tree representation.
These are ordinary Rust modules, not extra Cargo targets or test dispatchers.
Private implementation faults and input matrices remain in `src/` tests.
Public examples remain doctests on the existing exports.

## Coverage rules

- Fixed regressions retain exact errors and their existing configuration matrix.
- Generated namespace traces compare outcomes and complete visible trees after
  every operation and reopen. The native filesystem is not an oracle for errno
  ordering when multiple rejection conditions overlap.
- Successful lifecycle traces are positive controls, not random assumptions.
  Every generated trace must execute the intended successful transitions.
- Journals retain original first-touch states, including absence, rather than
  learning a new baseline from staged output. Namespace observations cannot
  disappear or duplicate across reopen.
- Property tests retain shrinking and source-local failure seeds. Keep properties
  in the entry files so moving helpers does not change seed-file discovery.

## Targeted runs

```sh
mise run test -- pvisor-overlay-core -- --test namespace_recovery
mise run test -- pvisor-overlay-core -- --test selective_lifecycle
mise run test -- pvisor-overlay-core -- --test content_receipts
mise run test -- pvisor-overlay-core -- --test backend_routing
mise run test -- pvisor-overlay-core -- --test recording_admission
PROPTEST_CASES=1024 PROPTEST_RNG_SEED=42 mise run test -- pvisor-overlay-core -- \
  --test namespace_recovery --test selective_lifecycle --test content_receipts
```
