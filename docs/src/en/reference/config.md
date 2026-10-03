# Configuration files

Save a working command as TOML to reuse its settings locally, in CI, and in batch jobs. Start with the offline example below: it keeps output in a Stage so you can inspect it before writing changes back to the project.

Use the [CLI reference](cli.md) for command-line options and the [policy reference](policy.md) for file and network rules.

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
| `filesystem` | `host` (default) / `sandbox` | `--filesystem`; top-level string, not a `[filesystem]` table |
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
| `overlaynet` | Mode, policy and rules | `mode` defaults to `auto`, `listen` to `127.0.0.1:19081`, `policy` to `public` |
| `gateway` | Optional capture/routes | Default mode `off`; capture requires Gateway feature |
| `record.destination` | Optional path | `--record-destination`; file or directory |
| `policies.session/workspace/user` | Policy layers | Strictest intersection |
| `container` | OCI settings | Default runtime `crun`, network `host`; image or rootfs |
| `vm` | VM settings | `memory_mib = 2048`, `cpus = 2`; macOS requires Linux rootfs/image |

Definitions come from `crates/pvisor/src/config.rs`. Internally resolved and `serde(skip)` fields are not configuration interfaces.

## Overrides and common mistakes

- Explicit CLI scalars replace config values; the command replaces `run.command`.
- Repeated list options generally replace the configured list. `--mount` and `--access` append; `--clear-access` clears preset/config rules.
- `--safe` presets sit between config and explicit CLI and clear configured pass_env; explicit `--pass-env` then applies.
- Unknown fields reject loading. Use `filesystem = "sandbox"`, not `[filesystem] mode = "sandbox"`.
- There is no general config-directory-relative path guarantee. Start from the intended workspace and use absolute paths across environments.


`[vm].memory_pool` is the socket path of the experimental macOS / Apple Silicon shared cold-page pool, unset by default. CLI uses `--vm-memory-pool SOCKET`; Rust SDK uses `VmSettings.memory_pool`. See [first-version memory sharing](../design/memory-sharing/index.md#v1-integration).

## Fields in commonly used groups {#settings}

### Run and resource limits

`[run].command` is a string array without automatic shell expansion. For pipes or redirection, explicitly use `/bin/sh -c`. Defaults are `executor = "host"`, `stdio = "inherit"`, `policy = "observe"`, and no `timeout_ms`.

`[run.resource_limits]` accepts the following optional integers. Omitting a field requests no limit for that dimension in the configuration.

| Field | Unit | CLI |
| --- | --- | --- |
| `memory_bytes` | Bytes | `--memory` |
| `processes` | Process/thread count | `--max-processes` |
| `cpu_time_ms` | Milliseconds of CPU time | `--max-cpu-time` |
| `open_files` | File descriptor count | `--max-open-files` |
| `file_size_bytes` | Bytes per file | `--max-file-size` |

Enforcement depends on the executor. Check requested, effective, mechanisms, and limitations under `resources` after execution. CPU time and wall-time timeout are separate settings.

### Files and networking

`[overlayfs].mount` and `access` are arrays of tables. Mount entries have `source`, optional `target`, and required `access`; access rules have `path` and `level`. Levels are `deny`, `ask`, `read`, `warn`, `stage`, and `write`. See [File policies](../guides/policies/files.md) for usage.

`[overlaynet].mode` accepts `auto`, `off`, or `proxy`; `policy` accepts `public`, `deny`, or `allowlist`. `allow` is an array of destination strings; `rules`, `deny`, and `limits` are structured table arrays. Lists default to empty. See [Policy fields](policy.md) for port, transport, and address rules.

### Container and VM

| Table | Fields and defaults |
| --- | --- |
| `container` | `runtime = "crun"`, `image = ""`, `network = "host"`, `read_only_rootfs = false`, `mounts = []` |
| Optional `container` fields | `rootfs`, `pvisor_binary`, `platform`, `workdir`, `user` |
| Each `container.mounts` entry | `source`, `target`, `read_only = false` |
| `vm` | `memory_mib = 2048`, `cpus = 2`, `rootfs_immutable = false`, `ram_compression = false` |
| Optional `vm` fields | `rootfs`, `image`, `image_store`, `library_dir`, `ram_backing`, `memory_pool` |

`container.platform` accepts `linux-amd64` or `linux-arm64`; `container.network` accepts `host`, `bridge`, or `none`. The injected Linux container binary must match the rootfs architecture and ABI.

VM memory is measured in MiB and CPU count is a positive integer. `ram_backing` retains a RAM file; `ram_compression` enables the corresponding compressed backing. Compressed backing and shared pools on macOS have additional FUSE requirements; see [Memory-sharing design](../design/memory-sharing/index.md).

### Capture and recording

`[gateway]` defaults to `mode = "off"`; capture uses `capture`. Other defaults are `admin_listen = "127.0.0.1:9876"`, `level = "dialogue"`, `session_header = "x-pvisor-session-id"`, `debug = false`, `stream_markdown = false`, and `routes = []`. The optional `profile` currently accepts `zcode-bigmodel`; `zcode_builtin_config` points to that integration's configuration. Configure routes and capture levels using the [Gateway guide](../guides/capture.md).

`[record].destination` is an optional path. Gateway model-traffic records and Trace Event journals have different responsibilities; retain the artifacts you need as described in [Jobs and storage](../concepts/jobs.md).
