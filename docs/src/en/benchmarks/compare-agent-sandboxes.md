# Comparison: agent-native sandboxes

Native sandboxes control one agent's tool permissions. pVisor adds shared staging, review, conflict protection and execution records across agents; the layers can be combined. Use [task overhead](agent-tasks.md) and [filesystem overhead](filesystem.md) for measured costs. This page does not rank unmeasured product performance.

## Scope

Official documentation checked on 2026-10-04. Installed locally: Claude Code 2.1.128 and Codex CLI 0.160.0. Gemini CLI was absent. Documentation capabilities below do not establish that these installed versions implement every current option. The [task report](agent-tasks.md) records the controlled CLI experiment.

| Option | Execution and network boundary | Workspace changes | Choose it when |
|---|---|---|---|
| Claude Code sandbox | OS boundary around shell commands and descendants; Seatbelt on macOS, bubblewrap on Linux, domain-checking proxy. File tools, MCP and hooks have separate permissions | Writes happen directly in permitted directories; command approval differs from later file admission | Claude Code is the primary interface and interactive permission configuration matters |
| Codex sandbox | read-only/workspace-write/danger-full-access, separate approval policy; Linux bubblewrap and macOS Seatbelt | Direct edits within workspace-write; worktrees support file parallelism | You want Codex-integrated approvals, rules and sessions |
| Gemini CLI sandbox | Seatbelt, Docker/Podman, runsc and other configurations; enforcement depends on runtime and settings | Container workspace mounts expose the corresponding files to writes | You primarily use Gemini and its tool/image configuration |
| pVisor | Host, isolated host, OCI, libkrun VM; host proxy and VM TCP enforcement differ; inspect the actual Bundle | Staged changes remain pending until apply; selective admission and preimage conflict protection | Several agents need one execution protocol, or review must precede workspace modification |

Sources: [Claude](https://code.claude.com/docs/en/sandboxing), [Codex](https://learn.chatgpt.com/docs/sandboxing), [Gemini](https://geminicli.com/docs/cli/sandbox/). pVisor evidence: [executors](../guides/executors/index.md), [review/apply](../guides/review-apply.md), [isolation tests](isolation-tests.md).

## Review and evidence

These native sandbox pages do not define pVisor's stage/preimage/apply-ledger protocol; agents can still have logs, diffs and Git workflows. pVisor records outcomes, observed controls and artifacts in a Run Bundle, with pending changes in a stage. Staging and access restrictions are configured independently: `--stage` alone permits access outside the staged workspace.

Our representative flow reads, edits, requests a model API, and then encounters a concurrent host edit. See [apply/drop](apply.md) for conflicts and [task overhead](agent-tasks.md) for CLI tool loops. Gemini execution and comparative native-sandbox performance were not measured in this edition.

## Complete-environment compatibility measurements {#reference-comparison}

Docker/CLI data in this section comes from the earlier shared-tool controlled batch. pVisor uses a prepared directory without an image. The new default `--rootfs host` versus complete Ubuntu comparison, internal tool times and current client pass/failure results are in [full Agent Env](agent-tasks.md#full-ubuntu); configurations and samples remain separate.

Real Claude/Codex CLIs complete controlled repair/test loops on native, staged, Docker, Firecracker and QEMU. Codex also passes 30/30 in pVisor VM, while Claude/VM initialization times out. This does not establish universal client compatibility. Codex uses uniform inner `danger-full-access`; Claude is limited to controlled Bash actions. Default inner sandboxes and their composition with outer isolation are not compared. See the [complete environment](agent-tasks.md#reference-env) for timing, failures and boundaries.


## Corrections

Use [pVisor issues](https://github.com/DeepLink-org/pvisor/issues), including version, configuration and an official source or reproduction. Append dated evidence when products change.
