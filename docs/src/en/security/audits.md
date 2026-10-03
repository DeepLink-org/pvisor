# Independent audits and penetration tests

No independent security audit or penetration-test report is currently registered here publicly. When assessing adoption, start with the executor boundaries, known limitations, and regression cases relevant to your configuration, then decide whether you need an independent assessment.

## Evidence you can inspect now {#evidence}

[Executor boundaries](executor-boundaries.md) describes platform controls, [Known limitations](known-limitations.md) states their conditions, and [Isolation tests](../benchmarks/isolation-tests.md) explains checks for specific attacks. Each Run Bundle reports installed controls.

These materials provide internal implementation and verification evidence. An independent audit requires an assessor, scope, and findings; passing internal tests does not replace that report.

## Define the assessment scope {#scope}

Fix the pVisor commit or version, operating system, executor, kernel, and FUSE/virtualization dependencies. List assets and actions to assess: workspace aliases and symlinks, traversal, staging and apply, process isolation, egress, credential projection, evidence accuracy, and resource exhaustion.

Test normal execution, setup failure, concurrent modifications, and crash recovery separately. Identify uncovered executors, protocols, tenancy models, and side channels so conclusions from one path are not extended to the whole product.

## Organize the report {#report}

| Field | Required content |
| --- | --- |
| Assessment | Assessor, dates, methods, and public report link |
| Implementation scope | Version/commit, platform, executor, configuration, and dependencies |
| Findings | Identifier, impact, severity, and reproduction conditions |
| Remediation | Open, mitigated, fixed, or accepted risk, with supporting evidence |
| Verification | Fix commit, retest date, and outcome |
| Uncovered scope | Untested attack surfaces, configurations, and deployments |

Record fixes and retests separately: merging a fix and assessor verification are different events. Preserve report history so adopters can match findings to the version they run.

## Report a problem {#reporting}

For boundary escapes or inaccurate evidence, privately submit version, command, expected behavior, and actual behavior through [Vulnerability disclosure](disclosure.md). Coordinate publication of details and remediation with the reporter.
