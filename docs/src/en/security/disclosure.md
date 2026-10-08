# Vulnerability disclosure policy

## How to report

**Do not disclose vulnerabilities in public issues.** Submit through GitHub private vulnerability reporting: [Report a vulnerability](https://github.com/DeepLink-org/pvisor/security/advisories/new) (repository Security → Advisories → Report a vulnerability).

Include:

- The pVisor version or commit, platform, and executor (host, container, or VM);
- the command or configuration you used, plus a Run Bundle when it is safe to share;
- the boundary you expected, and what the workload actually did;
- minimal reproduction steps, if you have them.

## Response targets

| Step | Target |
| --- | --- |
| Acknowledge receipt | Within 3 business days |
| Initial assessment and severity | Within 10 business days |
| Fix or mitigation plan | Shared with the initial assessment |
| Public advisory | After the fix ships, at a time coordinated with the reporter |

Advisories credit the reporter unless they ask to stay anonymous.

## Scope

**In scope**:

- A Run crosses a boundary its own capability evidence marks `Enforced`.
- Staged changes reach the workspace without `apply`.
- `apply` writes outside its target or overwrites a conflicting external change.
- Evidence misreports the controls actually installed.

**Out of scope**: limitations the documentation already states (for example, a bypassable cooperative host proxy), kernel bugs, side channels, and misuse of explicitly granted credentials. See [security overview](index.md) and [known limitations](known-limitations.md).

## Supported versions

Security fixes target the latest release and the `main` branch.

The repository root [`SECURITY.md`](https://github.com/DeepLink-org/pvisor/blob/main/SECURITY.md) matches this policy; changes must be kept in sync in both places.
