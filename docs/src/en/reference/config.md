---
status: todo
search:
  exclude: true
---

# Configuration files

!!! warning "Planned"
    The complete generated reference is pending. Current configuration information is in [CLI reference](cli.md).

## Question

Which fields, types, defaults, CLI equivalents, and merge rules does RunConfig accept?

## Requirements

- Automatically generate field tables from RunConfig serde definitions.
- Include TOML path, type, default, CLI option, scalar/list merge behavior.
- Identify values not honored on some CLI paths, such as run.inherit_env.

## Acceptance criteria

- Generate at docs build or check against code in CI.
- Cover `[run]`, top-level `filesystem`, `[overlayfs]`, `[overlaynet]`, `[gateway]`, `[record]`, `[policies.*]`, `[container]` and `[vm]`.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [CLI](cli.md), [policy fields](policy.md)

## Configuration entry points available today

`--config` reads TOML `RunConfig`; `--spec` reads resolved JSON `RunSpec`. They are different formats. Run files are explicit inputs; a project `run.toml` is not auto-loaded. Workspace `policy.toml` is automatic; see [policy fields](policy.md).

This host task writes a staged result outside the project and captures stdio:

```toml
filesystem = "sandbox"

[run]
executor = "host"
command = ["/bin/sh", "-c", "printf 'ready\n' > result.txt"]
timeout_ms = 30000
stdio = "capture"

[overlayfs]
stage = "../stage-config-001"
max_size = 67108864

[overlaynet]
mode = "proxy"
policy = "deny"
```

```bash
pvisor run --config run.toml
pvisor status --review ../stage-config-001
pvisor inspect ../stage-config-001 -- cat result.txt
```

Proxy deny applies to traffic reaching the proxy; this example does not establish mandatory offline execution. Add `--overlaynet-deny-all` to block ordinary egress through a mandatory boundary. Staging and filesystem sandboxing are independent.

## Field navigation (current implementation)

| TOML path | Type/default | CLI / purpose |
| --- | --- | --- |
| `filesystem` | `host` (default) / `sandbox` | `--filesystem`; top-level string, not a table |
| `run.executor` | `host` (default) / `container` / `vm` | `--executor` |
| `run.command` | String array, empty | Command after `--`; required to execute |
| `run.agent` | String, `agent` | `--name` |
| `run.timeout_ms` | Optional integer, milliseconds | `--timeout` takes duration strings |
| `run.stdio` | `inherit` / `capture` | `--stdio` |
| `run.policy` | `observe` / `enforce` | `--strict` requests mandatory admission |
| `run.inherit_env` / `run.pass_env` | Boolean / string array | CLI command adapters and safe rules resolve inheritance again; TOML alone does not prove credential projection |
| `run.resource_limits` | Optional integer budgets | `memory_bytes`, `processes`, `cpu_time_ms`, `open_files`, `file_size_bytes`; check effective evidence |
| `overlayfs.stage` / `max_size` | Optional path / bytes | `--stage` / `--overlayfs-max-size`; an overlayfs table requests staging |
| `overlayfs.mount` / `access` | Arrays of tables | `--mount` / `--access`; [file policy](../guides/policies/files.md) defines grammar |
| `overlaynet` | Mode, policy, rules | Defaults: `auto`, `127.0.0.1:19081`, `public` |
| `gateway` | Optional capture/routes | Default mode `off`; capture requires Gateway feature |
| `record.destination` | Optional path | `--record-destination`; file or directory |
| `policies.session/workspace/user` | Policy layers | Strictest intersection |
| `container` | OCI settings | Default runtime `crun`, network `host`; image or rootfs |
| `vm` | VM settings | `memory_mib = 2048`, `cpus = 2`; macOS requires Linux rootfs/image |

Definitions come from `crates/pvisor/src/config.rs`. This navigation table does not replace the complete generated table still planned above. Internally resolved and `serde(skip)` fields are not configuration interfaces.

## Overrides and common mistakes

- Explicit CLI scalars replace config values; the command replaces `run.command`.
- Repeated list options generally replace the configured list. `--mount` and `--access` append; `--clear-access` clears preset/config rules.
- `--safe` presets sit between config and explicit CLI and clear configured pass_env; explicit `--pass-env` then applies.
- Unknown fields reject loading. Use `filesystem = "sandbox"`, not `[filesystem] mode = "sandbox"`.
- There is no general config-directory-relative path guarantee. Start from the intended workspace and use absolute paths across environments.
