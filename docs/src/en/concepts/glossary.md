# Glossary

| Term | Meaning |
| --- | --- |
| [Job](jobs.md) | A persistent unit of work in pVisor: command, evidence, and staged changes |
| [Stage](staging.md) | A copy-on-write staging workspace |
| [apply / drop](staging.md) | Selectively merge staged changes into the target / discard them |
| [Run Bundle](../reference/run-bundle.md) | The result, control observations, artifacts, and summary of one run |
| [Capability](capabilities-and-evidence.md) | The request and actual control of one capability dimension (files, network, subprocess, …) |
| [Evidence](capabilities-and-evidence.md) | The controls an executor actually installed and the results observed, not the declaration |
| [Placement](../design/operations-events.md) | The chosen execution location (host / container / VM, and Overlay combinations) |
| Plan level | What the executor plans to install at admission: `Unsupported`, `Cooperative`, `Planned`—not proof it is installed |
| Observed level | What the executor observes at teardown: `Unenforced`, `Cooperative`, `Enforced` |
| [Interception](../guides/policies/network.md) | Network-layer interception of egress traffic (explicit proxy or VM data plane) |
| [L0–L3](../why/trust-ladder.md) | Trust ladder levels |

For mechanisms and fields, see [design](../design/index.md) and [reference](../reference/index.md).
