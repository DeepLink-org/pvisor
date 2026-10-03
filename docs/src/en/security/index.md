# Security overview

pVisor aims for **bounded, reversible, and auditable** execution: cap the blast radius of mistakes, let you discard file changes before apply, and record truthfully which controls actually took effect.

## At a glance

| Protects | Does not protect |
| --- | --- |
| Staged workspace: project files stay unchanged until apply, and conflicts refuse to overwrite your edits | External effects that already happened: remote API calls, database writes, sent messages |
| Sensitive paths: the `--safe` preset rejects `.ssh`, `.gnupg`, and private-key files inside the view | Secrets outside the view, explicitly shared, or renamed; keys in source and Git history |
| Mandatory network boundaries: VM `auto`, host `--overlaynet-deny-all`, container offline mode | Clients that ignore the proxy or open raw sockets under a cooperative proxy |
| Honest evidence: separates the request, the plan, the installed controls, and the observation | Kernel bugs, side channels, misuse of explicitly granted credentials |

Each Run's actual boundary comes from its own capability evidence, not from configuration, preset names, or executor names.

pVisor is neither cryptographic remote attestation nor hostile multi-tenant isolation.

- [Threat model](threat-model.md): assets, adversaries, trust boundaries, and out-of-scope threats.
- [Executor boundaries](executor-boundaries.md): what host, container, and VM protect per capability.
- [Hardening](hardening.md): progressively stricter configuration.
- [Known limitations](known-limitations.md): known gaps and invariant problems.
- [Vulnerability disclosure](disclosure.md): how to report privately, and response targets.
- [Third-party audits](audits.md)
