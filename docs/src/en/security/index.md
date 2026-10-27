# Security and support scope {#_1}

<span id="security-overview"></span>

pVisor aims for **bounded, reversible, and checkable** execution: cap the blast radius of mistakes, let you discard file changes before apply, and record truthfully which controls actually took effect.

## At a glance

| Protects | Does not protect |
| --- | --- |
| Staged workspace: project files stay unchanged until apply, and conflicts refuse to overwrite your edits | External effects that already happened: remote API calls, database writes, sent messages |
| Sensitive paths: the `--safe` preset rejects `.ssh`, `.gnupg`, and private-key files inside the view | Secrets outside the view, explicitly shared, or renamed; keys in source and Git history |
| Mandatory network boundaries: VM `auto`, host `--overlaynet-deny-all`, container offline mode | Clients that ignore the proxy or open raw sockets under a cooperative proxy |
| Honest evidence: separates the request, the plan, the installed controls, and the observation | Kernel bugs, side channels, misuse of explicitly granted credentials |

Each Run's actual boundary comes from its own capability evidence, not from configuration, preset names, or executor names.

pVisor is neither cryptographic remote attestation nor hostile multi-tenant isolation.

## Platforms, interfaces, and capability status {#support-scope}

When choosing an execution environment, check prerequisites in [Platforms and executor support](../reference/platforms.md), then verify that [Executor boundaries](executor-boundaries.md) cover the required file, network, and process controls. Native Jobs, standalone containers, and the daemon expose different interfaces. See the [integration matrix](../guides/infrastructure/integration.md) for native Job, Runtime, daemon, and shim capabilities, and the [CLI reference](../reference/cli.md) for standalone container commands.

Check requested policy, planned controls, and installed controls separately using [Capabilities, evidence, and guarantees](../concepts/capabilities-and-evidence.md). Experimental capabilities such as automatic wait management have their own [implementation stages](../design/experimental/index.md); experimental results do not establish support across all platforms or interfaces.

## Review the security of one task

List the files, credentials, networks, and external services available to the task. Establish the trusted side using the [Threat model](threat-model.md), and configure controls using [Hardening](hardening.md). After a native Job runs, use `pvisor status --review` to inspect installed controls, warnings, and file changes. Consult [Known limitations](known-limitations.md) before accepting results. Missing observations remain unknown; cooperative controls do not establish mandatory isolation.

## When you discover a boundary problem

If a task crosses a boundary its own evidence marks `Enforced`, or a record misreports controls, privately submit the version, executor, command, and minimal reproduction using [Vulnerability disclosure](disclosure.md). Do not disclose vulnerability details in a public issue.

No independent security audit report is currently registered publicly. Internal regressions and Run Bundles help check implementation and execution results but do not replace an independent assessment. See [Third-party audits](audits.md) for assessment scope and report requirements.
