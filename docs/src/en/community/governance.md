# Governance and maintainers

pVisor is currently decided by a single maintainer: code merges, releases, semspec approvals, and the authoritative documentation all go through the same entry point. There is no formal committee or multi-maintainer voting mechanism. This page describes current practice, not a charter that has yet to be established.

## How contributions land {#workflow}

1. Describe the user problem, expected behavior, and how to reproduce it; align on the problem and acceptance criteria in an issue first for substantial interface or behavior changes.
2. Submit the implementation, corresponding documentation, and verification results; run the relevant checks per [Contributing](contributing.md).
3. The maintainer reviews behavior, evidence, and tests before merging. Vulnerabilities touching security boundaries use the [private disclosure process](../security/disclosure.md).

Code review, merging, and semantic-spec approval are handled separately: a passing PR does not automatically approve a product commitment; new user-visible boundaries need corresponding specs and human review.

## semspec approval {#semspec}

Contributors may draft cases, fix implementations, and maintain runner tests. semspec approve/revoke and the real `REVIEWED.toml` and `.approved/` snapshots are executed only by the maintainer; AI does not perform approvals and does not weaken existing checks to obtain PASS. See [Testing and semspec](testing.md) for the commands and rules.

## Not yet established {#records}

A formal maintainer list, governance charter, and private conduct-reporting channel have not been established; they will be announced on this page once they are. Until then, design decisions are anchored by issue, PR, or ADR links, and discussion norms are in [Community](index.md).
