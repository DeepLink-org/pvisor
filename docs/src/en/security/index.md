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

## Review the security of one task

List the assets the task will touch: project files, HOME, credentials, host paths, network destinations, and external services. Trust the supervisor, host kernel, and executor backend; treat the Agent, scripts, and tools they launch as untrusted workloads. See the [Threat model](threat-model.md) for the full scope.

Choose controls per asset and check platform prerequisites. Project files need staging and apply conflict checks; file reads and network egress need the actual boundaries of the chosen executor. Grant task credentials only necessary permissions, and control allowed external writes with service-side permissions. Use [Executor boundaries](executor-boundaries.md) and [Hardening](hardening.md) to select configuration; a control in one dimension does not establish protection in another.

After execution, use `pvisor status --review` to check installed controls, warnings, and file changes. Inspect evidence for file reads, file writes, and networking separately. Keep missing observations unknown, and do not treat a cooperative proxy as mandatory isolation. Check [Known limitations](known-limitations.md) before accepting the result, then choose apply or drop; neither reverses external effects.

## When you discover a boundary problem

If a task crosses a boundary its own evidence marks `Enforced`, or a record misreports controls, privately submit the version, executor, command, and minimal reproduction using [Vulnerability disclosure](disclosure.md). Do not disclose vulnerability details in a public issue.

No independent security audit report is currently registered publicly. Internal regressions and Run Bundles help check implementation and execution results but do not replace an independent assessment. See [Third-party audits](audits.md) for assessment scope and report requirements.
