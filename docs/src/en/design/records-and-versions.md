# Records and version matrix

pVisor execution identity, fact logs, file publication and machine snapshots each own their records. One Session may contain schema-3 `run.json` with completion protocol 2, a schema-6 Bundle, version-5 Events and a version-4 Session. Those numbers constrain separate readers; they neither substitute for each other nor equal the Cargo package version.

## What each record establishes {#ownership}

| Record or protocol | Question answered | What it does not establish |
| --- | --- | --- |
| RunSpec / Operation | What is requested, with which effective policy and placement? | Installed controls or a started workload |
| `run.json` / execution Session | Which Execution, lifecycle, head and request ownership are current? | Successful external effects or atomic commit across all records |
| Bundle / Event Journal | What was observed, and which facts committed? | Complete machine restoration or exactly-once external requests |
| Stage / preimage / apply ledger | What are candidate files based on, and which target updates occurred? | Atomic multi-file visibility to external readers |
| Checkpoint / snapshot manifest | Which file or machine state can be restored under a profile? | Restoration across arbitrary hosts, binaries or architectures |
| Host / Guest AgentCtl | Which endpoint receives which control or cooperation requests? | Equal permissions or payloads merely because version numbers match |

Event IDs and Journal positions, Host `request_id`, Session lifecycle request IDs and checkpoint IDs occupy separate namespaces. Record correlation checks Session, Execution, generation and target ownership together rather than comparing one string. The [Execution model](execution-model.md) owns identities; [Operation and Event](operations-events.md) owns event relationships.

## Execution and fact records {#execution-records}

The matrix follows current source declarations and reader branches. Format changes require checking writers, readers, callers and existing records together. Detailed fields remain in their owning topics.

| Object | Current writer version | Reader checks | Source owner, under `crates/` |
| --- | --- | --- | --- |
| RunSpec | `schema_version = 1` | Runtime rejects mismatched versions; input validation does not establish execution success | `pvisor-core/src/execution.rs`, `pvisor/src/runtime/run.rs` |
| Operation | `version = 1` | Exact `OPERATION_VERSION` check plus identity, kind and rewrite validation | `pvisor-core/src/operation.rs` |
| Event | `version = 5` | Exact `VERSION` check; Observation `domain/name/version` separately identifies its payload | `pvisor-core/src/event.rs` |
| Journal Header / Record | `pvisor.trace/5` | Validates the Header and each Event | `pvisor-journal/src/journal.rs` |
| Run Bundle | `schema_version = 6` | Requires matching Run/Execution completion receipt and Bundle digest | `pvisor/src/runtime/bundle.rs` |
| RunRecord (`run.json`) | `schema_version = 3` | Completion protocol 2 binds the terminal result and Bundle; a dead owner without a receipt yields Unknown; live control is cleared at termination | `pvisor/src/runtime/registry.rs` |
| Execution completion receipt (`completion.json`) | `version = 2` | Binds `RunResult` and Bundle BLAKE3; missing or inconsistent data cannot establish success | `pvisor/src/runtime/completion.rs` |
| Task / Unix container Session (`session.json`) | `version = 4` | Validates version, profile and ownership bindings | `pvisor/src/runtime/execution/session.rs`, `pvisor/src/docker/session.rs` |

Event version 5 and Journal format 5 currently share the Event version constant. Other versions have no such relationship. An Observation payload change also cannot be judged compatible solely from outer Event version 5. Successful JSON parsing establishes syntax; each record still needs its own validation.

Public persisted fields are in [Run Bundle](../reference/run-bundle.md); event bytes, receipts and recovery are in [Journal](journal.md#format).

## File state and restore payloads {#state-records}

| Object | Current format | Reader and publication boundary | Source owner, under `crates/` |
| --- | --- | --- | --- |
| Workspace checkpoint | `schema_version = 3`, explicit `kind = workspace` | Reader requires schema 3; it is not interpreted as execution machine state | `pvisor/src/runtime/checkpoint.rs` |
| EnvironmentManifest | Writer selects 2, 3, 4 or 5 by profile | Checks RAM fields, file layout, digests and restore prerequisites | `pvisor/src/environment_snapshot/store.rs` |
| Immutable base seal | `version = 2` | Requires the digest-bound content index | `pvisor/src/environment_snapshot/base.rs` |
| Compact preimage log | `pvisor.preimages/2`, frame `PVR2` | First observations and completion records have independent contracts | `pvisor-overlay-core/src/preimage_log.rs`, `core.rs` |
| Stage durability / seal | `durability-v1`, `sealed-v1` | Missing policy retains the original strict contract; unknown policy is rejected; managed stages require complete seals for reuse or apply | `pvisor-overlay-core/src/stage.rs` |
| Apply ledger / ApplyRecord | Schema 2 for ordinary apply, schema 3 for project-scoped apply | Recovery follows recorded phases under the target lock | `pvisor-overlay-core/src/apply.rs` |
| `vm.ram` descriptor | `PVZRAM`, descriptor version 2 | Validates descriptor, absolute generation directory and head records; versioning is separate from environment manifests | `pvisor/src/ram_backing.rs` |

Environment manifest versions describe different RAM/filesystem combinations. A maximum version of 5 does not mean every new snapshot writes 5. Stage profiles use v4 for raw RAM and v5 for compressed RAM; other profiles follow their field combinations.

[Session checkpoint design](session-checkpoint-cli.md#10-当前实现与验收边界) owns entries and compatible profiles. [Environment snapshots](environment-snapshot.md#storage-contract) owns machine and file references, [Offload format](offload/disk-layout-and-schema.md) owns RAM subformats, and [Shared image cache](lazy-image-v2.md) owns image metadata/content versions.

## Control protocols and build identity {#control-protocols}

| Image index/content pool | `format_version=2`, `pvisor-radix-v2`, `pvisor-v2:` | Conditional HEAD update commits publication | `pvisor/src/image/cache/portable.rs`, `portable/`, `pool.rs` |
| Protocol | Version and additional bindings | Authority and scope |
| --- | --- | --- |
| Host AgentCtl envelope | `AGENTCTL_HOST_VERSION = 2` | Endpoint owners validate Session/Execution/generation targets; Guest tokens grant no Host authority |
| Guest AgentCtl | `AGENTCTL_VERSION = 1` | Workload `Hello`/`Sync`, state and cooperation; a separate protocol from Host |
| Session owner instance control | `pvisor-instance-control-3`, version-2 Host envelope, exact Session/Execution and recorded socket device/inode | Same-UID callers address the recorded owner socket |
| CLI owner bootstrap | Inherited private socketpair, internal DTO and same-binary BLAKE3 check | Only the frontend launches its own owner; subsequent commands do not use this DTO |
| Durable Session operation lock | `session.lock` diagnostic protocol `pvisor-session-lock-1`, PID and operation | Exclusive flock, 0600 and one hard link |
| Daemon native supervisor | Version-2 Host envelope, owner/token and exact target | Credentials and admission remain independent of the Session owner protocol |

[Host AgentCtl](architecture.md#host-wire) defines framing and socket
authority. [Rust API boundaries](architecture.md#api-boundaries) list public
crate entry points.

## Validate record bindings {#evolution}

1. Check the record's schema and required fields at its owning reader.
2. For persisted state, check digests, dependency references and Session,
   Execution and stage ownership. For live control, check the endpoint and
   recorded socket identity.
3. After timeout, disconnect or publication failure, locate the original
   request's commit point and reconcile it using [failure semantics](failure-semantics.md).
