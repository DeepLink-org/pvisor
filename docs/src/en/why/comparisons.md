# Fit check, comparisons, and FAQ

There is one test: **you are going to watch this execution from start to finish.** You do not need pVisor when:

- **You are watching it run**: approval and correction happen in the moment, so reviewing afterward adds nothing.
- **`git` already rolls it back cleanly**: if the diff is small and `git checkout` always leaves a clean tree, staging and selective merging do not save much.
- **The real side effects are external**: remote APIs, database writes, and sent messages are beyond staging's reach; rollback needs something else.
- **You need adversarial isolation**: for untrusted multi-tenancy you want a VM-grade isolation substrate, and pVisor is only one choice of executor.

When you want **unattended execution followed by review**, use pVisor to stage file changes, inspect execution records, and select the paths to apply.

## Comparing existing approaches

| Approach | Strong at | Additional setup or integration | When to choose it |
| --- | --- | --- | --- |
| Docker / devcontainer | Isolated environment, reproducible dependencies | Change review, refusing to overwrite on conflict, selective apply, evidence | You only need a reproducible environment |
| Agent-native sandbox | OS sandboxing, approvals and client session records | Unified cross-client stage/apply and executor-capability evidence require additional organization | Prefer native client protections first |
| git worktree | File-level parallelism | Network and credentials, execution evidence | File-only parallelism |
| Cloud sandboxes (E2B, Daytona, Modal) | Remote isolated execution | Local workspace integration and a local review flow | You need remote isolation |
| Kubernetes / Ray | Scheduling and orchestration | Task execution boundaries, file staging, and execution records | You already have an orchestration layer |
| gVisor / Firecracker / Kata | Isolation substrate | Execution semantics, review, and evidence | You need a stronger isolation substrate |

Existing worktrees, patch validation, and merge workflows can be used with Docker and `git diff`. Writes through a bind mount need their own recovery procedure. pVisor provides staging, selective submission, preimage conflict checks, and capability observations; the benchmarks show the runtime cost.

Assess overhead across the task flow: [end-to-end tasks](../benchmarks/agent-tasks.md) combines measured repair, filesystem tools, workspace review and CLI compatibility to analyze native Agent sandboxes, Docker/devcontainer, VMs and clouds. [Reinforcement learning](../benchmarks/compare-rl-infra.md) combines active-capacity and replay evidence to explain executor choices for rollout environment budgets. Cloud bills, default nested Agent sandboxes and complete training performance have no matched comparison.

## FAQ

**How does it differ from an agent-native sandbox?**
An agent-native sandbox answers whether it can block something. On top of that, pVisor provides retrospective selective merging, conflict protection, and a checkable execution record, with consistent semantics across agents and executors (see the table above).

**Does it slow agents down?**
pVisor adds the cost of admission, file interception, and recording. Measured startup and filesystem overhead is in [benchmarks and comparisons](../benchmarks/index.md) (partly under construction).

**Can it run in CI?**
Yes. You can use it in a pipeline the L1 way today: let the agent finish, review, and merge only what you want. Policy- and evidence-driven exemption and clustering (L2/L3) are the direction; see [running agents in CI](../guides/ci.md).

**Which agents are supported?**
Any command can run; Claude Code, Codex, and others get presets matched by executable name. See [connecting your agent](../guides/agents/index.md).

**Does data leave my machine?**
pVisor itself does not collect or upload usage data. But an agent's calls to model APIs leave the machine anyway, and pVisor does not keep them local: under `--safe`, only that agent's model API allowlist is permitted and other egress follows policy. pVisor's own network behavior appears only when you explicitly enable Gateway capture or use a remote image cache service. See [security overview](../security/index.md).

**Why does `last` not find the Job I just ran?**
With `--stage PATH`, the Job lives in the stage directory, while `last` searches default storage by workspace. Pass the path or Job ID explicitly. See [Jobs and storage](../concepts/jobs.md).

**Can staging undo external side effects?**
No. Staging covers files only; external API calls, database writes, and sent messages are not rolled back by it. See [staging and apply semantics](../concepts/staging.md).
