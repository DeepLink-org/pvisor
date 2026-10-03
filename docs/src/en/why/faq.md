# FAQ

**How does it differ from an agent-native sandbox?**
An agent-native sandbox answers whether it can block something. On top of that, pVisor provides retrospective selective merging, conflict protection, and a checkable execution record, with consistent semantics across agents and executors. See [comparisons](comparisons.md).

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
