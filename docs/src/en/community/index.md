# Community

pVisor is an Apache-2.0 open source project.

## Communication channels

| Purpose | Channel |
| --- | --- |
| Bugs and feature requests | [GitHub Issues](https://github.com/DeepLink-org/pvisor/issues) |
| Security vulnerabilities | [Private reporting](https://github.com/DeepLink-org/pvisor/security/advisories/new); see [Disclosure policy](../security/disclosure.md) |
| Code contributions | GitHub Pull Requests; see [Contributing](contributing.md) |

## Start with a reproducible problem

Include the version or commit, operating system, executor, minimal command, and expected and actual results in a report. Remove credentials and private content before sharing a Run Bundle or logs. Discuss substantial interface or behavior changes in an issue first, covering options, compatibility effects, and acceptance criteria before implementation.

Keep a PR focused on one change: repair the implementation, add relevant regressions, and update the authoritative Chinese and English documentation together. Read READMEs along the modified path, prepare dependencies using [Development environment](development.md), then follow [Contributing](contributing.md) for formatting, lint, and relevant tests. State which checks you actually ran and which conditions remain unverified; do not report unexecuted scenarios as passing.

## Separate validation from approval

Unit and integration tests check implementation. Semspec expresses product commitments as claims, violation examples, and executable checks. You may draft new cases, fix implementations, and maintain runner tests, but must not weaken existing claims, checks, or `xfail` annotations to obtain PASS.

Human semantic review is separate from tests and PR merging. AI must not run `semspec approve` / `revoke` or edit real `REVIEWED.toml` or `.approved/` snapshots. See [Testing and semspec](testing.md) for the rules. Contributions are licensed under Apache-2.0.
