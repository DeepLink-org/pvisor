# Configuration files

Save a working command as TOML to reuse its settings locally, in CI, and in batch jobs. Start with the local example below: it keeps output in a Stage so you can inspect it before writing changes back to the project.

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
- Repeated list options generally replace the configured list. `--mount` replaces the entire mount list; `--access` appends; `--clear-access` clears preset/config rules.
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
| Optional `vm` fields | `rootfs`, `image`, `image_store`, `library_dir`, `ram_backing`, `memory_pool`, `snapshot_filesystem_pool` |

`container.platform` accepts `linux-amd64` or `linux-arm64`; `container.network` accepts `host`, `bridge`, or `none`. The injected Linux container binary must match the rootfs architecture and ABI.

VM memory is measured in MiB and CPU count is a positive integer. `ram_backing` retains a RAM file; `ram_compression` enables the corresponding compressed backing. Compressed backing and shared pools on macOS have additional FUSE requirements; see [Memory-sharing design](../design/memory-sharing/index.md).

`[vm].snapshot_filesystem_pool` opts into immutable lower references during native capture of initial and restored VMs, including the first capture of different VMs. After the first seal, the live control connection retains verified owners; subsequent captures authenticate complete original lowers and reuse their sealed pool trees without broader runner access. Workers require an absolute host-owned path on the same volume as task stores, outside all VM-writable roots and snapshot stores. The first miss creates one pool copy per immutable id; concurrent misses serialize per id and hits create no temporary lower copies. Opt-in native v5 snapshots retain private file contents as independently owned 64 KiB compressed frames, reusing unchanged content; restores rebuild private writable inodes with complete metadata and hard-link topology. Live frame owners survive parent retirement/GC. Sealing checks decoded-content identities before compression, fully verifies pool hits and encodes only misses; this also applies to full compressed RAM sealing. Native capture encodes authenticated frozen private roots directly, without an intermediate private-tree copy; import, restore and suspended artifact export never open their recorded original paths. Complete data validation remains; latency/density benefits require measurement. This profile excludes networking, shared memory pools, ordinary RAM compression and explicit RAM backing. Retain the pool with its Job stores or export complete snapshots.

### Capture and recording

`[gateway]` defaults to `mode = "off"`; capture uses `capture`. Other defaults are `admin_listen = "127.0.0.1:9876"`, `level = "dialogue"`, `session_header = "x-pvisor-session-id"`, `debug = false`, `stream_markdown = false`, and `routes = []`. The optional `profile` currently accepts `zcode-bigmodel`; `zcode_builtin_config` points to that integration's configuration. Configure routes and capture levels using the [Gateway guide](../guides/capture.md).

`[record].destination` is an optional path. Gateway model-traffic records and Trace Event journals have different responsibilities; retain the artifacts you need as described in [Jobs and storage](../concepts/jobs.md).

## Complete configuration field reference {#all-fields}

Each row names a serialized field, its Rust type, its raw TOML/SDK default, and its purpose. `Option<T>` means optional; TOML omits the key instead of writing `null`. `Vec<T>` means an array; structs use tables. `required` applies when creating an array entry. Defaults shown here precede CLI presets and resolution.

`network_rule`, `bandwidth_limit`, `policy_layer`, and `network_layer` below are reusable entry shapes, not literal top-level TOML tables. Use network rules inside `overlaynet.rules`, `overlaynet.deny`, or a scoped network layer; limits inside their `limits` arrays. Each of `policies.session`, `policies.workspace`, and `policies.user` uses `policy_layer`.

Field names and types are checked against the Rust serde structures during the documentation build. Defaults and CLI behavior are reviewed separately; the coverage check does not replace that review.

<!-- config-fields:start -->
| TOML path / entry field | Rust type | Default | Purpose |
| --- | --- | --- | --- |
| `run` | `RunSettings` | `{}` | Command and process settings |
| `container` | `ContainerSettings` | `{}` | OCI executor settings; used when selected |
| `vm` | `VmSettings` | `{}` | VM executor settings; `kvm` is a legacy table alias |
| `filesystem` | `FilesystemMode` | `"host"` | `host` or `sandbox`; access control independent of staging |
| `overlayfs` | `Option<OverlayFsSettings>` | `unset` | Omitted: direct writes; even an empty table requests staging |
| `overlaynet` | `OverlayNetSettings` | `{}` | Network driver and base policy |
| `gateway` | `GatewaySettings` | `{}` | Model capture and routing |
| `record` | `RecordSettings` | `{}` | Trace Event journal destination |
| `policies` | `pvisor_core::SessionPolicies` | `{}` | Additional session, workspace and user restrictions |
| `run.agent` | `String` | `"agent"` | `--name`; CLI derives command name when left at default |
| `run.executor` | `RunExecutorKind` | `"host"` | `--executor`: host, container, vm |
| `run.timeout_ms` | `Option<u64>` | `unset` | Wall time in ms; `--timeout 30s` |
| `run.stdio` | `RunStdio` | `"inherit"` | inherit or capture; `--stdio` |
| `run.policy` | `RunPolicy` | `"observe"` | observe or enforce; `--strict` selects enforce |
| `run.inherit_env` | `bool` | `true` | Raw TOML/SDK default; CLI sets true only for command basename codex |
| `run.pass_env` | `Vec<String>` | `[]` | Environment names; `--pass-env KEY` replaces the list |
| `run.filesystem` | `Vec<FilesystemCapability>` | `[]` | Host capabilities outside the workspace; entry fields below |
| `run.resource_limits` | `ResourceLimits` | `{}` | Requested limits; compare effective values in the Bundle |
| `run.command` | `Vec<String>` | `[]` | Argument vector; command after `--` replaces it; no shell expansion |
| `container.runtime` | `PathBuf` | `"crun"` | OCI runtime executable; `--container-runtime` |
| `container.image` | `String` | `""` | OCI image reference; `--container-image` |
| `container.rootfs` | `Option<PathBuf>` | `unset` | Prepared rootfs instead of image; `--container-rootfs` |
| `container.pvisor_binary` | `Option<PathBuf>` | `unset` | Injected Linux executable; default current binary; `--container-pvisor-binary` |
| `container.platform` | `Option<ContainerPlatform>` | `unset` | linux-amd64 or linux-arm64; `--container-platform` |
| `container.network` | `ContainerNetwork` | `"host"` | host, bridge, none; `--container-network` |
| `container.workdir` | `Option<PathBuf>` | `unset` | Container cwd when no Run cwd is mounted; `--container-workdir` |
| `container.user` | `Option<String>` | `unset` | uid, uid:gid, or name; `--container-user` |
| `container.read_only_rootfs` | `bool` | `false` | Read-only image root; `--container-read-only-rootfs` |
| `container.mounts` | `Vec<ContainerMount>` | `[]` | Bind mounts; repeated `--container-mount` replaces list |
| `container.mounts[].source` | `PathBuf` | `required` | Host path |
| `container.mounts[].target` | `PathBuf` | `required` | Container path |
| `container.mounts[].read_only` | `bool` | `false` | Read-only bind mount |
| `vm.ram_backing` | `Option<PathBuf>` | `unset` | New RAM backing path; existing files rejected; `--vm-ram-backing` |
| `vm.ram_compression` | `bool` | `false` | Seekable compressed backing; `--vm-ram-compression` |
| `vm.memory_pool` | `Option<PathBuf>` | `unset` | Experimental macOS pool socket; `--vm-memory-pool` |
| `vm.snapshot_filesystem_pool` | `Option<PathBuf>` | `unset` | Host-owned immutable snapshot lower pool; Linux x86-64 no-network private-RAM profile; config/SDK only |
| `vm.rootfs` | `Option<PathBuf>` | `unset` | Linux directory; CLI defaults to host `/` on Linux; `--rootfs` |
| `vm.image` | `Option<String>` | `unset` | OCI image instead of a rootfs directory; `--rootfs IMAGE` |
| `vm.image_store` | `Option<PathBuf>` | `unset` | OCI cache path; `--vm-image-store` |
| `vm.rootfs_immutable` | `bool` | `false` | Reject applying changes into the rootfs lower |
| `vm.library_dir` | `Option<PathBuf>` | `unset` | Firmware directory; musl embeds firmware and rejects it; `--vm-library-dir` |
| `vm.memory_mib` | `u32` | `2048` | Guest RAM in MiB; CLI `--memory` also sets a process budget |
| `vm.cpus` | `u16` | `2` | vCPU count; `--cpu` |
| `overlayfs.mount` | `Vec<FilesystemMount>` | `[]` | Unified mounts; repeated `--mount` replaces list |
| `overlayfs.access` | `Vec<FilesystemAccessRule>` | `[]` | Access rules; repeated `--access` appends; `--clear-access` clears |
| `overlayfs.stage` | `Option<PathBuf>` | `unset` | New durable Stage outside project; `--stage` |
| `overlayfs.max_size` | `Option<u64>` | `unset` | Aggregate staged bytes; `--overlayfs-max-size` |
| `overlayfs.mount[].source` | `PathBuf` | `required` | Host source path |
| `overlayfs.mount[].target` | `Option<PathBuf>` | `unset` | Agent-visible mount path; normalization supplies it when omitted |
| `overlayfs.mount[].access` | `FilesystemAccessLevel` | `required` | deny, ask, read, warn, stage, write |
| `overlayfs.access[].path` | `String` | `required` | Agent-visible path/rule; use the file policy guide grammar |
| `overlayfs.access[].level` | `FilesystemAccessLevel` | `required` | deny, ask, read, warn, stage, write |
| `overlaynet.mode` | `OverlayNetMode` | `"auto"` | auto, off, proxy; `--overlaynet`; auto uses smoltcp in VM |
| `overlaynet.listen` | `String` | `"127.0.0.1:19081"` | Proxy address; CLI chooses a free port for this default; `--overlaynet-listen` |
| `overlaynet.policy` | `OverlayNetPolicy` | `"public"` | public, deny, allowlist; `--overlaynet-policy` |
| `overlaynet.allow` | `Vec<String>` | `[]` | Legacy destination string grants; prefer structured rules |
| `overlaynet.rules` | `Vec<NetworkAccessRule>` | `[]` | Structured grants; network_rule fields below; `--overlaynet-rule` replaces |
| `overlaynet.deny` | `Vec<NetworkAccessRule>` | `[]` | Structured denials; `--overlaynet-deny` replaces |
| `overlaynet.limits` | `Vec<NetworkBandwidthLimit>` | `[]` | Bandwidth entries; bandwidth_limit fields below; `--overlaynet-limit` replaces |
| `gateway.mode` | `GatewayMode` | `"off"` | off or capture; `--gateway-mode` |
| `gateway.profile` | `Option<GatewayProfile>` | `unset` | zcode-bigmodel; `--gateway-profile` selects capture |
| `gateway.zcode_builtin_config` | `Option<PathBuf>` | `unset` | Zcode integration configuration path |
| `gateway.admin_listen` | `String` | `"127.0.0.1:9876"` | Admin address; CLI chooses a free port for this default; `--gateway-admin-listen` |
| `gateway.level` | `CaptureLevel` | `"dialogue"` | summary, dialogue, full; `--gateway-level` |
| `gateway.session_header` | `String` | `"x-pvisor-session-id"` | Session correlation header; `--gateway-session-header` |
| `gateway.debug` | `bool` | `false` | Gateway debugging; `--gateway-debug` |
| `gateway.stream_markdown` | `bool` | `false` | Render streamed dialogue; `--gateway-stream-markdown` |
| `gateway.routes` | `Vec<ModelRoute>` | `[]` | Model route entries; `--gateway-route` replaces list |
| `record.destination` | `Option<PathBuf>` | `unset` | Trace Event file/directory; `--record-destination` |
| `run.resource_limits.memory_bytes` | `Option<u64>` | `unset` | Bytes; `--memory` |
| `run.resource_limits.processes` | `Option<u64>` | `unset` | Process/thread count; `--max-processes` |
| `run.resource_limits.cpu_time_ms` | `Option<u64>` | `unset` | CPU milliseconds; `--max-cpu-time`; distinct from wall timeout |
| `run.resource_limits.open_files` | `Option<u64>` | `unset` | File descriptors; `--max-open-files` |
| `run.resource_limits.file_size_bytes` | `Option<u64>` | `unset` | Bytes per file; `--max-file-size` |
| `run.filesystem[].path` | `String` | `required` | Host path outside the staged project |
| `run.filesystem[].access` | `FilesystemAccess` | `required` | read or read_write |
| `network_rule.host` | `String` | `required` | Hostname, wildcard suffix, IP or CIDR; no URL scheme |
| `network_rule.ports` | `Vec<u16>` | `[]` | Port numbers 1–65535; empty means all ports |
| `network_rule.transports` | `Vec<NetworkTransport>` | `[]` | http, https, tcp_tunnel; empty means all transports |
| `network_rule.allow_private_ips` | `bool` | `false` | Allow hostname resolution to private/loopback addresses |
| `bandwidth_limit.host` | `Option<String>` | `unset` | Omitted matches every intercepted destination |
| `bandwidth_limit.port` | `Option<u16>` | `unset` | Omitted matches every port |
| `bandwidth_limit.bytes_per_second` | `u64` | `required` | Positive byte rate; matching limits stack |
| `gateway.routes[].name` | `String` | `required` | Model pattern: exact, prefix*, *suffix, * |
| `gateway.routes[].provider` | `Option<String>` | `unset` | openai, anthropic, gemini, vertex, bedrock, azure, copilot, custom |
| `gateway.routes[].upstream` | `Option<String>` | `unset` | Upstream API base including prefix, e.g. /v1 |
| `gateway.routes[].upstream_anthropic` | `Option<String>` | `unset` | Anthropic API base; falls back to upstream |
| `gateway.routes[].api_key_env` | `Option<String>` | `unset` | Host variable name holding a key; avoids a literal in TOML |
| `gateway.routes[].api_key` | `Option<String>` | `unset` | Literal upstream key; configuration must be kept private |
| `gateway.routes[].forward` | `Option<String>` | `unset` | Exact route name to forward to; rewrites model |
| `policies.session` | `PolicyLayer` | `{}` | This Session's additional policy layer |
| `policies.workspace` | `PolicyLayer` | `{}` | Explicit layer or defaults from .pvisor/policy.toml |
| `policies.user` | `PolicyLayer` | `{}` | Explicit layer or defaults from user configuration |
| `policy_layer.network` | `Option<NetworkPolicyLayer>` | `unset` | Optional network_layer table; absent adds no scoped constraint |
| `policy_layer.filesystem` | `Option<FileAccessPolicy>` | `unset` | Optional deny/ask/warn/allow glob arrays; see policy reference |
| `network_layer.default_action` | `Option<NetworkDefaultAction>` | `unset` | allow or deny; absent denies unmatched targets when this layer exists |
| `network_layer.allow` | `Vec<NetworkAccessRule>` | `[]` | network_rule grant entries |
| `network_layer.deny` | `Vec<NetworkAccessRule>` | `[]` | network_rule denials; deny wins |
| `network_layer.limits` | `Vec<NetworkBandwidthLimit>` | `[]` | bandwidth_limit entries |
<!-- config-fields:end -->
