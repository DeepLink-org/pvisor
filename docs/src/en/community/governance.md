# Governance and maintainers

Choose an entry by change size: submit a PR for wording fixes and small issues, and discuss larger interface or behavior changes in an issue with clear acceptance criteria. Report security-boundary vulnerabilities through [private disclosure](../security/disclosure.md).

## Current contribution and decision workflow {#workflow}

1. Describe the user problem, expected behavior, and reproduction.
2. For design changes, explain options, costs, and compatibility effects, with clear acceptance requirements.
3. Submit implementation, matching documentation, and validation, following [Contributing](contributing.md).
4. Reviewers check behavior, evidence, and tests; maintainers with the relevant repository permissions merge and release changes.

Code review, merge permission, and semantic approval are separate responsibilities. Passing PR tests does not automatically approve a product commitment; new public boundaries need matching specifications and human review.

## Human responsibility for semspec approval {#semspec}

Contributors can draft cases, fix implementations, and maintain runner tests. Maintainers assess claim accuracy and coverage before approve/revoke actions and approval-record updates. AI does not approve cases, edit real `REVIEWED.toml` or `.approved/` snapshots, or weaken existing checks to obtain PASS.

Provide source version, environment, results, and limitations with the review. See [Testing and semspec](testing.md) for commands and rules.

## Becoming a sustained contributor {#contributors}

Start with a reproducible problem and maintain a module's implementation, documentation, and regression cases over time. Propose broader responsibilities in a public issue, describing the intended scope and work already completed.

Repository permissions determine who can merge and release; expanded responsibilities need confirmation from maintainers with that authority. When views differ, collect the disputed points, alternatives, and validation in one issue and request a maintainer decision linked to the resulting change.

## What governance records should publish {#records}

Maintainer entries should include accounts, responsibilities, and effective dates. Design decisions should link to issues, PRs, or ADRs; permission and release-responsibility changes should remain recorded. Sensitive security reports follow disclosure procedures, with public records referencing only publishable conclusions.
