# Environment variables

Usually you only need to choose where Runs are stored and explicitly pass the credentials your agent needs. Host variables configure pVisor itself; environment projection determines what the task receives.

```bash
PVISOR_RUN_HOME="$HOME/.pvisor/runs" pvisor run --safe \
  --stage ../stage-env-001 --overlaynet-deny-all -- /bin/sh -c 'printf "%s\n" "$PVISOR_RUN_ID"'
```

The task prints its own Run ID. For network credentials, use `--pass-env` as described in [Credentials and environment](../guides/policies/credentials.md).

## Common host settings

| Variable | Current behavior |
| --- | --- |
| `PVISOR_RUN_HOME` | Default Run root; otherwise `~/.pvisor/runs`, or system temporary storage without HOME |
| `PVISOR_IMAGE_STORE` | OCI cache override; explicit `--image-store` wins |
| `PVISOR_CACHE_SERVER` | Shared cache endpoint; `off` disables; unset probes the per-user Unix socket |
| `PVISOR_CACHE_TOKEN` | Shared secret for TCP cache endpoints; do not pass it to agents with `--pass-env` |
| `PVISOR_BIN` | Obsolete Python-launcher override; installed native scripts do not read it. Invoke the binary path directly or select it through `PATH` |
| `XDG_CONFIG_HOME` | User-policy root, default `~/.config` |
| `HOME` / `PATH` | Host storage/tool-discovery/projection inputs; safe mode redirects HOME |

See [shared cache](shared-image-cache.md) for endpoint grammar, security, and failures. Host settings and agent-visible variables are separate; use `--pass-env NAME` for explicit projection.

## Daemon credentials {#daemon}

`OPEN_SANDBOX_API_KEY` supplies the standalone `pvisor-daemon serve` lifecycle key. Use a protected random secret of at least 32 bytes; do not pass it in argv or project it to workloads. Clients send it as the `OPEN-SANDBOX-API-KEY` HTTP header. The environment name uses underscores; the HTTP name uses hyphens.

Default daemon endpoints return a sandbox-scoped `X-PVISOR-SANDBOX-TOKEN` in endpoint headers; clients must preserve it. It is not a host environment setting or a lifecycle credential. See [endpoint authentication](../guides/daemon/operations.md#endpoints). Native cache/pool credentials and Job environment projection remain separate. Retired Cluster tokens are not daemon credentials.

## Runtime injection

`PVISOR_RUN_ID`, `PVISOR_RUNTIME`, `PVISOR_STORAGE`, `PVISOR_AGENT`, and `PVISOR_ROLE` identify runtime context. `PVISOR_AGENTCTL_ENDPOINT`, `PVISOR_AGENTCTL_TOKEN`, `PVISOR_AGENTCTL_TRANSPORT`, and `PVISOR_AGENTCTL_VERSION` support the optional cooperative Guest AgentCtl channel. The token is a Guest-only credential: it cannot authorize Host Job, VM or daemon-supervisor operations and should not be logged.

Proxy mode also injects upper/lowercase `HTTP_PROXY`, `HTTPS_PROXY`, and `ALL_PROXY`. These direct cooperating clients; they do not establish mandatory isolation alone. The Bundle `environment` field lists the actual projected names.

`PVISOR_KRUN_RUNNER_SPEC` and `PVISOR_KRUN_NETWORK_FD` are internal launch protocol. `PVISOR_KRUN_LOG` and `PVISOR_KRUN_ENOMEM_WORKAROUND` are VM diagnostics, not stable configuration. Use the tables above to configure the host; leave internal launch variables to the launcher and diagnostic tools.

## Startup diagnostics and source builds {#build-and-diagnostics}

`PVISOR_STARTUP_TIMING` enables startup phase logs by default; set it to `0` to disable them for measurements without logging overhead. It changes diagnostic output, not task policy.

The following configure the guest kernel embedded **at build time** in Linux x86_64 musl builds:

| Variable | Input | Precedence |
| --- | --- | --- |
| `PVISOR_KRUNFW_KERNEL_BUNDLE` | Directory containing `kernel.bin` and `kernel.json` | Takes precedence when set |
| `PVISOR_KRUNFW_PATH` | `libkrunfw.so.5` file from which to extract the kernel | Used when the bundle is unset |

Rebuild the CLI after changing these inputs. Setting them while running an already-built musl binary does not replace its embedded kernel. See [Platform support](platforms.md) for dynamic firmware entry points and platform requirements. The build implementation is `crates/pvisor-vm/build_kernel.rs`.
