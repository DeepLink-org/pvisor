# 0004: Passing a test and approving a commitment are separate actions {#adr-0004}

**Status: existing contribution rule backfill.** This records the existing contract without inferring independent maintainer approval.

**Context.** A passing test establishes one successful verification. A public commitment also needs human judgment about scope and conditions.

**Options.** Automatically approve passing cases, or retain a separate human review step.

**Current choice.** AI and contributors can draft cases, fix implementations, and maintain runner tests. Humans maintain `semspec approve`, `revoke`, real `REVIEWED.toml` ledgers, and `.approved/` snapshots. Existing checks and xfail annotations must not be weakened to obtain PASS. See [Testing and semspec](../../community/testing.md).

**Consequences.** Reviews consider claims, execution results, and human approval records together. Green tests do not automatically expand support.
