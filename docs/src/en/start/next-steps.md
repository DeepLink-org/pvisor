# After your first run

Replace the demo with a narrowly scoped real task, such as changing one implementation under `src`. Retain the current repository baseline, confirm the agent is installed in the selected execution environment, and configure its model service and credentials. pVisor does not select the agent's provider.

## Connect a real agent

For an installed Codex using an explicitly delivered OpenAI key, run from the project directory. Set `OPENAI_API_KEY` in the host environment first and use a fresh stage outside the project each time:

```bash
pvisor run --safe --pass-env OPENAI_API_KEY --stage ../agent-task-001 -- codex
pvisor status --review ../agent-task-001
pvisor inspect ../agent-task-001 -- git --no-pager diff -- src
```

`--safe` matches network presets by the direct executable filename. Adding `--overlaynet-allow` replaces the preset list, so list model APIs alongside dependency downloads or other model services. HOME writes are discarded after exit; do not depend on this run to persist login state. See [Agent integration](../guides/agents/index.md) for other agents' credentials and prerequisites.

## Decide whether to keep, retry, or discard

After the Job stops, check its exit code, installed controls, warnings, and changes; a nonzero exit can still leave candidates. `inspect` requires an OverlayFS workspace and uses host tools, not tools inside the original VM or container.

Apply selected paths when satisfied. To retry from file state, workspace fork requires a stopped host Job with reconstructible launch policy; VM/container Jobs or Jobs with Gateway routes do not support this path. Parent and child stages must share a filesystem, and you must fork before a full apply/drop cleans up staging data. See [review and apply](../guides/review-apply.md) for conflict handling.

## Before expanding the task

Selective proxies on Linux host remain cooperative. For mandatory selective egress, prepare a VM with the agent installed. See [credentials and environment](../guides/policies/credentials.md) for credential delivery methods.

When adding concurrency, give each task a separate worktree and stage, select results by explicit paths, and review each one separately. Do not apply concurrently into the same target tree. The [parallel workspace workflow](../guides/parallel-agents.md#batch) uses two offline tasks to verify records and merging before substituting real agents.
