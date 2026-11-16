# Governance and maintainers

One maintainer currently handles pVisor's code merges, releases, specification approvals and final documentation decisions. The project has no formal committee, multi-maintainer voting process or governance charter.

## How contributions land {#workflow}

1. Describe the user problem, expected behavior, and how to reproduce it; align on the problem and acceptance criteria in an issue first for substantial interface or behavior changes.
2. Submit the implementation, corresponding documentation, and verification results; run the relevant checks per [Contributing](contributing.md).
3. The maintainer reviews behavior, evidence, and tests before merging. Vulnerabilities touching security boundaries use the [private disclosure process](../security/disclosure.md).

Code review, merging, and semantic-spec approval are handled separately: a passing PR does not automatically approve a product commitment; new user-visible boundaries need corresponding specs and human review.

## Semantic specification approval {#semantic-review}

Contributors may draft cases, fix implementations and maintain fixture tests. Specification approval/revocation and edits to real `REVIEWED.toml` and `.approved/` snapshots are human-only maintainer operations; AI does not perform approvals or weaken checks to obtain PASS. Product checks now use Scrut, but maintainers have not yet explicitly decided its replacement digest-review policy. Entries requiring review refuse execution in the meantime. See [Testing and Scrut](testing.md) for the rules.

## Not yet established {#records}

A formal maintainer list, governance charter, and private conduct-reporting channel have not been established; they will be announced on this page once they are. Until then, design decisions are anchored by issue, PR, or ADR links, and discussion norms are in [Community](index.md).
