# Reproduce the Run lifecycle

[`examples/`](https://github.com/DeepLink-org/pvisor/tree/main/examples) follows the pVisor CLI. Each `run.sh` manages its own `.work/` directory and reports persistent outputs. Examples follow documentation order: execute first, then govern effects.

```bash
just examples
```

## pVisor

| Example | Description |
| --- | --- |
| `01-filesystem-isolation` | Transactional workspace isolation |
| `02-changeset-management` | Review, apply and discard |
| `03-network-isolation` | Explicit proxy policy and its boundaries |
| `04-gateway-llm-control` | Embedded Gateway routing and capture |

Requires macOS or Linux, Cargo, Python 3 and common POSIX tools such as `jq`. Filesystem examples also require macFUSE/FUSE3. Run `just examples 01-filesystem-isolation 02-changeset-management` for FUSE-dependent examples 01/02, or `just examples 03-network-isolation 04-gateway-llm-control` for examples 03/04 without FUSE.

Start with `pvisor/01-filesystem-isolation`, then changeset management.

See [Guides](../guides/index.md) for tasks. Examples validate product paths; [CLI reference](../reference/cli.md) defines exact command syntax.
