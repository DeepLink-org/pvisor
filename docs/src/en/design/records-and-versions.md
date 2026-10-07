# Records and version matrix

pVisor execution identity, fact logs, file publication and machine snapshots each own their records. One Job may contain schema-1 `run.json`, a schema-4 Bundle, version-5 Events and a version-2 execution Job. Those numbers constrain separate readers; they neither substitute for each other nor equal the Cargo package version.

## What each record establishes {#ownership}

| Record or protocol | Question answered | What it does not establish |
| --- | --- | --- |
| RunSpec / Operation | What is requested, with which effective policy and placement? | Installed controls or a started workload |
| `run.json` / execution Job | Which Attempt, lifecycle, head and request ownership are current? | Successful external effects or atomic commit across all records |
| Bundle / Event Journal | What was observed, and which facts committed? | Complete machine restoration or exactly-once external requests |
| Stage / preimage / apply ledger | What are candidate files based on, and which target updates occurred? | Atomic multi-file visibility to external readers |
| Checkpoint / snapshot manifest | Which file or machine state can be restored under a profile? | Restoration across arbitrary hosts, binaries or architectures |
| Host / Guest AgentCtl | Which endpoint receives which control or cooperation requests? | Equal permissions or payloads merely because version numbers match |

Event IDs and Journal positions, Host `request_id`, Job lifecycle request IDs and checkpoint IDs occupy separate namespaces. Record correlation checks Job, Attempt, generation and target ownership together rather than comparing one string. The [Execution model](execution-model.md) owns identities; [Operation and Event](operations-events.md) owns event relationships.

## Execution and fact records {#execution-records}

The matrix follows current source declarations and reader branches. Format changes require checking writers, readers, callers and existing records together. Detailed fields remain in their owning topics.

| Object | Current writer version | Reader and compatibility boundary | Source owner, under `crates/` |
| --- | --- | --- | --- |
| RunSpec | `schema_version = 1` | Runtime rejects mismatched versions; input validation does not establish execution success | `pvisor-core/src/execution.rs`, `pvisor/src/runtime/run.rs` |
| Operation | `version = 1` | Exact `OPERATION_VERSION` check plus identity, kind and rewrite validation | `pvisor-core/src/operation.rs` |
| Event | `version = 5` | Exact `VERSION` check; Observation `domain/name/version` separately identifies its payload | `pvisor-core/src/event.rs` |
| Journal Header / Record | `pvisor.trace/5` | Header and Event validation work together; complete old formats are rejected rather than truncated as incomplete tails | `pvisor-journal/src/journal.rs` |
| Run Bundle | `schema_version = 4` | Requires current schema and observation contracts; default zeroes cannot stand in for missing observations | `pvisor/src/runtime/bundle.rs` |
| RunRecord (`run.json`) | `schema_version = 1` | Reader checks schema 1; optional/default fields follow current serde definitions | `pvisor/src/runtime/registry.rs` |
| Execution Job (`execution-job.json`) | `version = 2` | Exact version and Job/root/stage binding checks; no automatic old-record conversion | `pvisor/src/runtime/job_execution.rs` |

Event version 5 and Journal format 5 currently share the Event version constant. Other versions have no such relationship. An Observation payload change also cannot be judged compatible solely from outer Event version 5. Successful JSON parsing establishes syntax; each record still needs its own validation.

Public persisted fields are in [Run Bundle](../reference/run-bundle.md); event bytes, receipts and recovery are in [Journal](journal.md#format).

## File state and restore payloads {#state-records}

| Object | Current format | Reader and publication boundary | Source owner, under `crates/` |
| --- | --- | --- | --- |
| Workspace checkpoint | `schema_version = 3`, explicit `kind = workspace` | Reader requires schema 3; it is not interpreted as execution machine state | `pvisor/src/runtime/checkpoint.rs` |
| EnvironmentManifest | Writer selects 2, 3, 4 or 5 by profile | Reader accepts explicit v1–v5/RAM-field combinations and checks file layout, digests, platform conditions and `SnapshotCompatibility` | `pvisor/src/environment_snapshot/store.rs` |
| Immutable base seal | `version = 2` | Requires the digest-bound content index; old seals are rejected | `pvisor/src/environment_snapshot/base.rs` |
| Compact preimage log | `pvisor.preimages/2`, frame `PVR2` | First observations and completion records have independent contracts; legacy per-file journals use their existing reader without forced in-place conversion | `pvisor-overlay-core/src/preimage_log.rs`, `core.rs` |
| Stage durability / seal | `durability-v1`, `sealed-v1` | Missing policy retains the original strict contract; unknown policy is rejected; managed stages require complete seals for reuse or apply | `pvisor-overlay-core/src/stage.rs` |
| Apply ledger / ApplyRecord | Current schema 2; reader accepts 1 or 2 | Compatibility does not invent new guarantees for old records; recovery follows recorded phases under the target lock | `pvisor-overlay-core/src/apply.rs` |
| `vm.ram` descriptor | `PVZRAM`, descriptor version 2 | Validates descriptor, absolute generation directory and head records; versioning is separate from environment manifests | `pvisor/src/ram_backing.rs` |

Environment manifest versions describe different RAM/filesystem combinations. A maximum version of 5 does not mean every new snapshot writes 5. Stage profiles use v4 for raw RAM and v5 for compressed RAM; other profiles follow their field combinations. SDK readability of a storage format does not mean the current Job CLI can adopt arbitrary historical standalone snapshot stores.

[Job checkpoint design](job-checkpoint-cli.md#10-当前实现与验收边界) owns entries and compatible profiles. [Environment snapshots](environment-snapshot.md#storage-contract) owns machine and file references, [Offload format](memory-optimization/offload-format.md) owns RAM subformats, and [Shared image cache](shared-image-cache-storage.md) owns image metadata/content versions.

## Control protocols and build identity {#control-protocols}

| Protocol | Version and additional bindings | Authority and compatibility scope |
| --- | --- | --- |
| Host AgentCtl envelope | `AGENTCTL_HOST_VERSION = 1` | Endpoint owners validate Job/Attempt/generation targets; Guest tokens grant no Host authority |
| Guest AgentCtl | `AGENTCTL_VERSION = 1` | Workload `Hello`/`Sync`, state and cooperation; a separate protocol from Host |
| CLI Job listener / worker ticket | `pvisor-job-ticket-4`, plus Cargo package version and executable BLAKE3 | Internal exact schema/build match; rebuilding the same package version may be incompatible |
| Daemon native supervisor | Version-1 Host envelope, owner/token and exact target | Uses neither Guest protocol nor CLI worker tickets; no transparent legacy-supervisor adoption |

Source entries are `pvisor-core/src/host_protocol.rs`, `protocol.rs`, `pvisor-cli/src/cli/host_service.rs` and `pvisor/src/runtime/supervisor.rs`. [Host AgentCtl](architecture.md#host-wire) owns wire framing, same-UID boundaries and upgrade order. Rust `api` organization and wire compatibility are separate concerns; see [API boundaries and migration status](architecture.md#api-boundaries).

## Relationships to check during version changes {#evolution}

1. Identify whether the change affects an input DTO, stored object, domain payload or live protocol, and locate its actual reader.
2. Explicitly choose old-format acceptance, conversion or rejection. Changing a version number cannot bypass structural or binding checks.
3. For stored objects, check digests, dependency references, stage/Attempt ownership and restore profile. For live protocols, also check authority, generation, tickets and build identity.
4. Handle active requests and instances belonging to old listeners/supervisors before upgrades. File compatibility does not establish live-process takeover.
5. After timeout, disconnect or publication failure, locate the original request's commit boundary and reconcile it using [Failure semantics and retries](failure-semantics.md) before retrying.

