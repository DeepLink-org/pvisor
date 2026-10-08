# 0001: A plan cannot prove that controls were installed {#adr-0001}

**Status: implementation backfill.** This records the existing contract without inferring independent maintainer approval.

**Context.** Before launch, pVisor can plan how a requested policy should be implemented. Permissions, platforms, and startup failures can prevent installation.

**Options.** Let plans assert enforcement, or wait for executor observations.

**Current choice.** Admission plans reach at most `Planned`; executor observations supply actual enforcement evidence. The Run Bundle retains requests, plans, and observations. See [Evidence design](../../concepts/capabilities-and-evidence.md).

**Consequences.** Automation must inspect actual observations, and platform adapters must report installation outcomes. Task completion or a configuration field does not replace that evidence.
