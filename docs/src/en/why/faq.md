# FAQ

## How does pVisor differ from agent sandboxes?

Beyond blocking actions, it answers what changed (staged manifest), what to keep (selective apply with conflict refusal), and what can be checked (installed controls and denied accesses). Claude Code, Codex, and scripts share the same semantics. See [comparisons](comparisons.md).

## How does it differ from Docker plus `git diff`?

Docker isolates the environment and diff shows changes. Conflict protection, recoverable apply batches, installed-control evidence, and default sensitive-path protection need additional machinery. See [staging semantics](../concepts/staging.md).

## Does it slow agents down?

Only VM guest initialization currently has measured data: about 114 ms ready p50 on Apple M4/HVF. See [startup](../benchmarks/startup.md). Filesystem and end-to-end measurements remain in progress in [benchmarks](../benchmarks/index.md).

## Which agents work?

Any command can run. Claude Code, Codex, Gemini CLI, and ZCode have `--safe` model-API presets. Other commands deny outbound access by default and need explicit grants. See [agent integration](../guides/agents/index.md).

## Does data leave my machine?

pVisor does not collect or upload usage data. Bundles and capture remain local; image/firmware features download their inputs. Agent traffic follows your policy; `--safe` presets allow the corresponding model APIs. See [network policy](../guides/policies/network.md) and [threat model](../security/threat-model.md).

## Can an agent read SSH private keys?

`--safe` rejects `.ssh`, `.gnupg`, and common private-key names within the workspace view and gives the agent a private HOME. Executor boundaries govern other paths. Linux overlay rules alone do not hide secrets at original paths outside that view; use `--filesystem sandbox` or a VM for a stronger boundary. See [executor boundaries](../security/executor-boundaries.md).

## Can external API calls be undone?

No. `apply` and `drop` manage staged workspace files only. Remote calls, database writes, and messages are irreversible through them. See [staging](../concepts/staging.md#不可逆的部分).

## Can it run in CI?

Yes: `pvisor run` returns the workload exit code. The CI integration guide remains [planned](../guides/ci.md).

## Why does `last` not find my Job?

It searches default storage for the current workspace only. With `--stage PATH`, select the stage path or Job ID explicitly. See [Jobs and storage](../concepts/jobs.md).
