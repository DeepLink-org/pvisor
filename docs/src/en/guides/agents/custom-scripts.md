# Scripts and automation commands

pVisor requires no agent: any command can run inside the boundary with the same staging, review, and evidence.

```bash
pvisor run --safe --overlaynet-deny-all -- ./agent.sh
pvisor status --review last
pvisor apply last --path src
```

## Behavior

- **Working directory:** the command's working directory is the staged view, which contains the project's current files; writes go to the stage.
- **Exit code:** the command's exit code is returned unchanged as the `pvisor run` exit code, so scripts and CI can judge success or failure.
- **Review failures too:** when the command exits nonzero, its file changes stay in the stage for you to review and apply or drop.
- **Arguments:** everything after `--` is passed through unchanged as the command and arguments.

## Networking

When a script matches no agent preset, `--safe` denies all outbound access. Choose as needed:

| Need | Option |
| --- | --- |
| Fully offline | `--overlaynet-deny-all` |
| Only a few destinations | `--overlaynet-allow pypi.org:443` (repeatable) |
| Reject a destination | `--overlaynet-deny 169.254.0.0/16` |

When used as command names, `bash`, `sh`, `zsh`, and `fish` reuse the Codex preset destinations; see [`--safe` presets](../../reference/cli.md#safe-参数预设).

## Environment variables

`--safe` projects only the necessary variables. Pass the variables a script needs explicitly with `--pass-env NAME`; see [credentials and environment](../policies/credentials.md).

## Complete example

[Your first run](../../start/first-run.md) uses a "fake agent" script to demonstrate modifications, deletion, a denied sensitive read, and blocked networking. The runnable examples under [`examples/pvisor/`](https://github.com/DeepLink-org/pvisor/tree/main/examples/pvisor) cover file isolation, changeset management, and network isolation.
