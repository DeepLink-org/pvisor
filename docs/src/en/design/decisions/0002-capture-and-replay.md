# 0002: Capture and replay have separate data responsibilities {#adr-0002}

**Status: implementation backfill.** This records the existing contract without inferring independent maintainer approval.

**Context.** Running tasks, recording model traffic, and restoring native agent context require different protocols and dependencies.

**Options.** Put every protocol in the execution core, or exchange explicit records across independent components.

**Current choice.** Gateway owns model traffic capture and routing; Replay consumes native agent trajectories and re-executes tools; the execution core maintains Run lifecycle and shared records. Gateway is an optional feature, and Replay has a separate crate and command. See [Replay design](../replay.md).

**Consequences.** Users can use execution and staging alone. Capture journals and native trajectories must be collected separately; new agent integrations primarily belong in the Replay adapter boundary.
