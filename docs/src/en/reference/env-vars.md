---
status: todo
search:
  exclude: true
---

# Environment variables

!!! warning "Planned"
    The complete reference is pending. See [Credentials and environment](../guides/policies/credentials.md) for variables projected to agents.

## Question

Which `PVISOR_*` variables does pVisor read (for example `PVISOR_RUN_HOME` and `PVISOR_CACHE_SERVER`), and which does it inject into agents?

## Requirements

- Collect every read site from the code and generate two tables automatically: variables pVisor reads and variables injected into agents.
- Describe each variable: purpose, default, applicable executors and platforms, and stability.

## Acceptance criteria

- A script generates the tables and CI detects new `PVISOR_*` read sites that are missing from them.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [CLI](cli.md)

## Common host settings

| Variable | Current behavior |
| --- | --- |
| `PVISOR_RUN_HOME` | Default Run root; otherwise `~/.pvisor/runs`, or system temporary storage without HOME |
| `PVISOR_IMAGE_STORE` | OCI cache override; explicit `--image-store` wins |
| `PVISOR_CACHE_SERVER` | Shared cache endpoint; `off` disables; unset probes the per-user Unix socket |
| `PVISOR_CACHE_TOKEN` | Shared secret for TCP cache endpoints; do not pass it to agents with `--pass-env` |
| `PVISOR_BIN` | Binary override for the Python launcher; usually unnecessary |
| `XDG_CONFIG_HOME` | User-policy root, default `~/.config` |
| `HOME` / `PATH` | Host storage/tool-discovery/projection inputs; safe mode redirects HOME |

See [shared cache](shared-image-cache.md) for endpoint grammar, security, and failures. Host settings and agent-visible variables are separate; use `--pass-env NAME` for explicit projection.

## Runtime injection

`PVISOR_RUN_ID`, `PVISOR_RUNTIME`, `PVISOR_STORAGE`, `PVISOR_AGENT`, and `PVISOR_ROLE` identify runtime context. `PVISOR_AGENTCTL_ENDPOINT`, `PVISOR_AGENTCTL_TOKEN`, `PVISOR_AGENTCTL_TRANSPORT`, and `PVISOR_AGENTCTL_VERSION` support cooperation. The token is a credential and should not be logged.

Proxy mode also injects upper/lowercase `HTTP_PROXY`, `HTTPS_PROXY`, and `ALL_PROXY`. These direct cooperating clients; they do not establish mandatory isolation alone. The Bundle `environment` field lists the actual projected names.

`PVISOR_KRUN_RUNNER_SPEC` and `PVISOR_KRUN_NETWORK_FD` are internal launch protocol. `PVISOR_KRUN_LOG` and `PVISOR_KRUN_ENOMEM_WORKAROUND` are VM diagnostics, not stable configuration. An automatic inventory remains planned; this table covers common user settings.
