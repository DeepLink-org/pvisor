# Architecture decision records (ADR)

These records cover choices already reflected in pVisor's implementation and development workflow. They explain the current system for design review and carry the status “implementation backfill”.

| Number | Choice | Record status |
| --- | --- | --- |
| 0001 | Separate admission plans from installed enforcement evidence | Implementation backfill |
| 0002 | Give Gateway and Replay independent boundaries | Implementation backfill |
| 0003 | Preserve executor selection under safe; reject missing controls | Implementation backfill |
| 0004 | Human approval establishes semantic commitments | Existing contribution rule backfill |
| 0005 | One Rust VM crate and a uniform trait API | Implementation backfill |

## 0001: A plan cannot prove that controls were installed {#adr-0001}

**Context.** Before launch, pVisor can plan how a requested policy should be implemented. Permissions, platforms, and startup failures can prevent installation.

**Options.** Let plans assert enforcement, or wait for executor observations.

**Current choice.** Admission plans reach at most `Planned`; executor observations supply actual enforcement evidence. The Run Bundle retains requests, plans, and observations. See [Evidence design](../../concepts/capabilities-and-evidence.md).

**Consequences.** Automation must inspect actual observations, and platform adapters must report installation outcomes. Task completion or a configuration field does not replace that evidence.

## 0002: Capture and replay have separate data responsibilities {#adr-0002}

**Context.** Running tasks, recording model traffic, and restoring native agent context require different protocols and dependencies.

**Options.** Put every protocol in the execution core, or exchange explicit records across independent components.

**Current choice.** Gateway owns model traffic capture and routing; Replay consumes native agent trajectories and re-executes tools; the execution core maintains Run lifecycle and shared records. Gateway is an optional feature, and Replay has a separate crate and command. See [Replay design](../replay.md).

**Consequences.** Users can use execution and staging alone. Capture journals and native trajectories must be collected separately; new agent integrations primarily belong in the Replay adapter boundary.

## 0003: Safe expresses requirements while executor selection stays explicit {#adr-0003}

**Context.** Users choose executors for speed, Linux userspace, or kernel isolation while expecting their file and network policies to take effect.

**Options.** Safe could silently select an executor, or impose requirements on the chosen executor.

**Current choice.** `--safe` configures staging, file access, environment, and related settings without selecting an executor. Missing required controls cause startup rejection. See [Network policies](../../guides/policies/network.md) for network paths.

**Consequences.** Identical arguments can encounter capability errors on different machines. Install the prerequisites or explicitly select an executor that meets the requirements; reviewers inspect the Bundle for the outcome.

## 0004: Passing a test and approving a commitment are separate actions {#adr-0004}

**Context.** A passing test establishes one successful verification. A public commitment also needs human judgment about scope and conditions.

**Options.** Automatically approve passing cases, or retain a separate human review step.

**Current choice.** AI and contributors can draft cases, fix implementations, and maintain runner tests. Humans maintain `semspec approve`, `revoke`, real `REVIEWED.toml` ledgers, and `.approved/` snapshots. Existing checks and xfail annotations must not be weakened to obtain PASS. See [Testing and semspec](../../community/testing.md).

**Consequences.** Reviews consider claims, execution results, and human approval records together. Green tests do not automatically expand support.

## 0005: Keep VM backend differences inside one Rust runtime {#adr-0005}

**Status.** Implementation backfill; this records the implemented boundary, not independent maintainer approval.

**Context.** VM configuration, device customizations and snapshots were split between pVisor and multiple libkrun crates. C contexts, borrowed raw pointers and public backend structs made ownership and platform differences leak into consumers.

**Options.** Retain an external C context ABI and vendored component crates, or consolidate private runtime modules behind one Rust contract.

**Current choice.** `pvisor-vm` compiles the core modules together. Only `api` is public; portable structs and traits define every public method signature without conditional compilation or default implementations. Private adapters implement those traits. Architecture layout and hypervisor behavior have separate internal traits; callers receive explicit unsupported errors and inspect capabilities. Firmware data ABI and OS FFI remain private.

**Migration and consequences.** Executor, shim, snapshot/checkpoint/pager, examples and the init benchmark use the new API. Hardware/device tests move inside the crate. Static kernel packing belongs to the runtime. Old core crate dependencies and VM control C functions are removed; existing evidence identifiers and serialized snapshot fields remain compatible. Uniform API shape does not promise cross-architecture restore. `api_contract` and `repository_boundary` guard the boundary; native tests and target compile checks supply distinct evidence. See [development](../../community/development.md) and the [runtime contract](https://github.com/deeplink-org/pvisor/blob/main/crates/pvisor-vm/README.md).

## Record the next decision {#new-decision}

Use a unique four-digit number. Formal standalone records follow `NNNN-short-title.md`; the existing directory structure is preserved here. Each record needs context, options, decision, consequences, status, and links to implementation or replacement decisions.

Distinguish proposed, accepted, superseded, and rejected. Merged code alone does not establish maintainer ADR approval. For changes to boundaries or record formats, describe migration and validation.
