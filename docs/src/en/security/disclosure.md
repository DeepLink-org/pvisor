# Vulnerability disclosure

Keep this policy synchronized with repository [SECURITY.md](https://github.com/DeepLink-org/pvisor/blob/main/SECURITY.md).

## Reporting

**Do not disclose vulnerabilities in public issues.** Use [GitHub private vulnerability reporting](https://github.com/DeepLink-org/pvisor/security/advisories/new): Security → Advisories → Report a vulnerability.

Include version/commit, platform, executor, command/config, expected boundary, observed violation, and a minimal reproduction. Attach a Bundle only when safe to share.

## Response targets

| Step | Target |
| --- | --- |
| Acknowledge | 3 business days |
| Assessment/severity | 10 business days |
| Fix/mitigation plan | With assessment |
| Public advisory | After a fix, coordinated with reporter |

Reporters are credited unless they request anonymity.

## Scope

In scope: crossing controls reported Enforced; staged writes reaching workspace without apply; apply escaping targets/overwriting conflicting edits; evidence misreporting installation.

Excluded: documented cooperative-proxy limits, kernel bugs, side channels, misuse of granted credentials. See [overview](index.md) and [limitations](known-limitations.md).

## Supported versions

Fixes target the latest release and main.
