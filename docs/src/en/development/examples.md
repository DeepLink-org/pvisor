# Reproduce the Run lifecycle

The [`examples/`](https://github.com/DeepLink-org/Persisting/tree/main/examples)
directory is organized by the pVisor CLI. Each `run.sh` manages its own `.work/`
directory and reports durable outputs. Together they follow the documented
sequence: execute, then govern effects.

```bash
just examples
just examples-pvisor
```

## pVisor

| Example | What it demonstrates |
|---|---|
| `01-filesystem-isolation` | Transactional workspace isolation |
| `02-changeset-management` | Review, apply, and drop |
| `03-network-isolation` | Explicit proxy policy and its boundary |
| `04-gateway-llm-control` | Embedded Gateway routing and capture |

Requirements are macOS or Linux, Cargo, Python 3, and common POSIX tools such
as `jq`. The filesystem examples additionally require macFUSE or FUSE3.
`just examples-pvisor-filesystem` runs the FUSE-backed 01/02 scenarios;
`just examples-pvisor-portable` runs 03/04 without FUSE.

Start with `pvisor/01-filesystem-isolation`, then continue to changeset
management.

Use [pVisor Guides](../guides/index.md) for task explanations.
The examples verify a product workflow; exact command syntax remains in the
[CLI reference](../reference/cli.md).
