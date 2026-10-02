---
status: todo
search:
  exclude: true
---

# Third-party audits and penetration tests

!!! warning "Planned"
    No data yet. Below are the requirements; contributions are welcome.

## Question

Have independent audits or penetration tests been performed, and what did they find?

## Requirements

- Metric: audit scope, findings, and remediation status.
- Control group: —
- Workload: —
- Environment: —

## Acceptance criteria

- Publish the audit scope, date, and report link.
- Mark each finding with its remediation status.
- Explain the uncovered scope.

## Tracking

- Tracking issue: TODO
- Owner: TODO
- Related: [security overview](index.md), [vulnerability disclosure](disclosure.md)

## Report format

An audit record includes at least the auditor, date, pVisor commit, platform and executor, test configuration, attack surface, finding severity, fix commits, and retest results. Untested executors, protocols, tenant models, and resource attacks must each be listed.

There is no independent audit conclusion today; internal testing and semspec results do not substitute for a third-party audit, and the absence of a public report does not imply that no issues were found. Handle vulnerability details under the [disclosure policy](disclosure.md) before publishing.
