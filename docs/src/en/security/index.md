# Security overview

pVisor aims for **bounded, recoverable, checkable** execution: limit blast radius, discard file changes before apply, and honestly record installed controls. It provides neither cryptographic remote attestation nor hostile multi-tenant isolation.

## At a glance

| Protects | Does not protect |
| --- | --- |
| Staged workspace stays unchanged before apply; conflicts refuse overwrites | Prior remote APIs, database writes, messages |
| `--safe` rejects .ssh/.gnupg/private-key names within the view | Outside-view/shared/renamed secrets, embedded keys, Git history |
| VM auto, host `--overlaynet-deny-all`, container offline mandatory networking | Direct sockets or ignored proxies under cooperative networking |
| Evidence separates request, plan, installation, observation | Kernel bugs, side channels, misuse of granted credentials |

The individual Run's evidence defines its boundary, not configuration or executor names.

## Pages

- [Threat model](threat-model.md): assets, adversaries, trust, exclusions.
- [Executor boundaries](executor-boundaries.md): capability scope.
- [Hardening](hardening.md): progressively narrower permissions.
- [Known limitations](known-limitations.md): gaps and invariant issues.
- [Disclosure](disclosure.md): private reporting and response targets.
- [Audits (planned)](audits.md).
