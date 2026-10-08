# 0008: Separate Host and Guest control authority {#adr-0008}

**Status: implementation backfill.** Records current protocol and ownership boundaries without expanding platform-validation scope.

## Context {#context}

Guests report state, receive directives and participate in checkpoint quiescence. Host callers manage Jobs, VMs, stages and supervisors. Their trust scopes differ, so guest cooperation credentials cannot grant host lifecycle authority.

## Alternatives and choice {#decision}

Host operations could be added to Guest `Hello`/`Sync`, or use separate endpoints, schemas and credentials. The implementation chooses separation: Guest AgentCtl retains workload cooperation, while Host envelopes carry typed host requests whose endpoint owners validate Job/Attempt/generation and authority.

The CLI Job listener accepts requests under a private same-UID authority root, handing off through separate tickets and worker channels. Native daemon supervisors use Host envelopes with owner/token credentials, not Guest tokens or CLI worker tickets. Shared Core defines envelopes and validation; implementations retain CLI DTOs and transport.

## Consequences {#consequences}

Bundle `agentctl` state still describes Guest cooperation, not Host authorization receipts. Both protocols currently use version 1, but are not interchangeable. CLI worker schema, package version and executable digest jointly determine internal compatibility; old listeners/supervisors follow their drain-and-upgrade contract.

Host `request_id` provides correlation rather than durable deduplication for every command. Timeout or cancellation requires reconciling Job state and effects. Private directories and same-UID peer checks define host trust scope rather than isolating all other same-UID processes. macOS identity/descendant cleanup and real VM TUI paths retain the documented validation gaps.

## Implementation and scope {#implementation}

Source entries: `crates/pvisor-core/src/host_protocol.rs`, `protocol.rs`; `crates/pvisor-cli/src/cli/host_service.rs`, `host_fds.rs`; `crates/pvisor/src/runtime/host_transport.rs`, `supervisor.rs`, `instance_control.rs`.

[Host/Guest AgentCtl](../architecture.md#host-agentctl) owns detailed authority and upgrades, the [Protocol matrix](../records-and-versions.md#control-protocols) lists versions, and [Failure semantics](../failure-semantics.md#idempotency) explains disconnects and retries.
