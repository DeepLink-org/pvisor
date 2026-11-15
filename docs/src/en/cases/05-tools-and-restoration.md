# 5. Replay trajectories and choose restoration tools

Choose whether you need a file proposal, Agent history, or full machine execution state. Workspace checkpoints save files; pvisor-replay handles Agent-native trajectories; Session execution checkpoints save full state for supported VMs. Use review/apply/drop for staged files. Gateway handles model routing and capture, and cache/memory-pool serve execution infrastructure.

### S-USE-015: Prepare a trajectory without rerunning historical tools

Before resuming an Agent conversation, use prepare-only to check that prefix parsing does not execute historical tools. Install pvisor-replay alongside pvisor.

**Contract**: The prefix is prepared without creating marker; replayed_tool_calls is zero.

**Violation**: prepare-only executes historical commands.

Execution and assertions: [Scrut S-USE-015](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/replay/S-USE-015.md).

### S-USE-016: Check full execution checkpoint capabilities

The host Session in this example does not support CPU/RAM save and restore; compatible native VM Sessions do. Check capability and require explicit rejection instead of silently falling back to workspace fork.

**Contract**: Execution checkpoint, suspend, resume, and execution fork all report CAPABILITY_UNSUPPORTED and leave the Session tree unchanged.

**Violation**: An execution option is ignored or a refusal changes the Session.

Execution and assertions: [Scrut S-USE-016](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-USE-016.md).

[Learning path](index.md)
