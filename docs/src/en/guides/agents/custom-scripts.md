# Scripts and automation commands

pVisor requires no agent: any command can use the same staging, review, and evidence.

```bash
pvisor run --safe --overlaynet-deny-all -- ./agent.sh
pvisor status --review last
pvisor apply last --path src
```

## Behavior

- **Working directory:** the staged view contains the current project; writes enter staging.
- **Exit code:** run returns the workload's code for script/CI decisions.
- **Failure review:** nonzero exits still retain staged changes for apply/drop.
- **Arguments:** everything after `--` is passed as the command and arguments.

## Networking

Without an agent preset, `--safe` mode denies outbound access.

| Need | Option |
| --- | --- |
| Offline | --overlaynet-deny-all |
| Specific destinations | --overlaynet-allow pypi.org:443, repeatable |
| Reject a destination | --overlaynet-deny 169.254.0.0/16 |

Executables bash, sh, zsh, and fish use the Codex destinations; see [`--safe` presets](../../reference/cli.md#safe-参数预设).

## Environment

`--safe` mode projects necessary variables only. Pass required names with `--pass-env NAME` explicitly; see [credentials](../policies/credentials.md).

## Complete examples

[First run](../../start/first-run.md) demonstrates modifications, deletion, sensitive denial, and blocked networking. [Repository examples](https://github.com/DeepLink-org/pvisor/tree/main/examples/pvisor) cover files, changesets, and network isolation.
