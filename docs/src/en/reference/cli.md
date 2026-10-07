# `pvisor` command reference

Job is pVisor's primary user-facing object. `pvisor run` creates a Job; the other
flat commands operate on it directly, without adding a `job` subcommand, and
`replay` creates a Job from a trajectory. See the
[execution model](../design/execution-model.md) for how Job, Run and Attempt
relate; existing Job IDs and on-disk records keep their `run-*`
and `Run Bundle` names.
Full command examples for Host, OCI VM and transparent host-rootfs VM
are in
[Run workloads with pVisor](../guides/executors/index.md).

## Host Job service {#host-agentctl}

Built-in Job commands (`run`, `status`, `kill`, `suspend`, `resume`, `fork`,
`checkpoint`, `inspect`, `review`, `apply`, `drop`) submit typed requests through
Host AgentCtl. The first request starts a persistent same-user listener under
`/tmp/pvisor-host-<effective-UID>` (using canonical `/tmp`, usually `/private/tmp`
on macOS); later requests reuse it. You do not need to start a service or pass
endpoint options for ordinary persisted Jobs. Bare `pvisor` still displays help;
`pvisor -- COMMAND` follows default execution through the same Job service.

The frontend retains its terminal and launches a listener-authorized worker;
stdio and the private worker channel use Unix `SCM_RIGHTS` descriptor transfer.
Job service/internal-worker JSON uses shared `runtime/host_transport.rs`
async/sync newline framing with a 1 MiB JSON limit excluding the delimiter;
FD marker bytes are separate transport records, not JSON. The listener checks
same-UID kernel credentials and agrees on Host version 1, the internal Job
ticket schema, package version and executable content digest before
admitting stdio/commands. The typed `JobCommand` payload contains internal CLI
DTOs tied to that exact schema/build, not a stable public API. It is not the node/cache/pool service managed by `pvisor service`.
Guest AgentCtl `Hello`/`Sync` remains a separate cooperative channel: its token
cannot authorize host Job or VM operations.

Drain active requests and stop an old listener with its old binary before
upgrading. A new client refuses an incompatible live listener; there is no
legacy fallback. Lost responses and cancellation can leave effects ambiguous;
requests are not automatically retried. See [Host and Guest AgentCtl](../design/architecture.md#host-agentctl)
for ownership, protocol and validation limits.

## Experimental features {#features}

List runtime features without starting or contacting the Host Job service:

```bash
pvisor feature
pvisor feature list --json
pvisor --feature workload-aware-memory-offloading feature --json
```

Each entry includes `name`, `stage`, `default`, `enabled` and `description`. `default` is the registered default; `enabled` reflects that default plus this query's `--feature` options. The query does not load a Run configuration or inspect a live Job. An enabled flag does not certify platform availability or runtime success.

Use the repeatable global `--feature NAME` option before or after `run`; comma-separated names are also accepted. Unknown names reject. Arguments after `--` belong to the workload.

```bash
pvisor run --executor vm --feature workload-aware-memory-offloading -- /bin/sleep 10
```

| Feature | Stage | Default | Scope |
|---|---|---|---|
| `workload-aware-memory-offloading` | `experimental` | `false` | EXP-001 M0 native VM vCPU observation; automatic offload is not implemented |

`workload-aware-memory-offloading` requires the final executor to be VM and a Linux x86_64/KVM or Apple Silicon macOS/HVF build. It does not replace an executor selection. HVF runtime validation remains outstanding. Observation is enabled through the native runner; no live Host observation query is exposed yet.

Feature enables apply to `run` and feature queries. Other lifecycle commands reject them; execution resume/fork retain the saved configuration. `[features]` in an explicit Run configuration can also enable features; CLI enables override `false`, omission preserves the configuration. There is no CLI disable option or persistent `feature enable` action. See [feature configuration](config.md#features). Runtime features are separate from Cargo build features and do not change existing VM memory flags.

## Service commands {#service}

Top-level commands operate native Jobs. `service` manages native node resources and dispatches installed companions. `run/status/restart/stop --config FILE` manage configured native roles; the single-node sandbox daemon has its own lifecycle API and persistent state.

```bash
pvisor service --help
pvisor service daemon --help
pvisor service cache --help
pvisor service memory-pool --help
```

Use `service cache/memory-pool` for the native resource tools. `service daemon` passes arguments unchanged to a separately installed, matching adjacent `pvisor-daemon`; check `pvisor service --help` when using an older build. The daemon can always be invoked directly after separate installation; follow the [daemon installation guide](../guides/daemon/index.md). NativeRuntime embeds VM execution; the daemon executable and required native flags are integrated, checkpoint/fork and stage/apply APIs are absent, and node sharing is not automatically acquired. Controller/Worker task tools and their configuration are retired. See [service entry points](../guides/daemon/service.md) for native node/cache/pool ownership and deployment boundaries.

Other unknown names follow default execution rules. The `ctrl` name
is an explicit exception: `pvisor ctrl`, `pvisor ctrl --help` and
`pvisor help ctrl` reject with migration guidance before Job admission, rather
than default-running a program named `ctrl`. Use the live VM commands below.
`pvisor run -- ctrl` still expresses explicit workload intent; it does not
invoke a control API. Use `pvisor -- COMMAND` for explicit default
execution.

## Find the command you need

- **Run a command:** start with [`pvisor run`](../start/first-run.md), then use
  `status --review`, `inspect` and `apply` to decide which changes reach the project.
- **Understand the execution boundary:** use `status` and `inspect`, then read the
  [executor guides](../guides/executors/index.md).
- **Continue a trajectory:** use `replay` only when you already have a supported
  trajectory, after reading the [replay guide](../guides/replay.md).

For your first run, copy the smallest loop:

```bash
pvisor run --safe -- codex
pvisor status --review last
pvisor apply last --path src
```

`last` resolves only Jobs that belong to the current workspace in default
storage. With `--stage PATH`, pass that path or the Job ID to later commands;
see [Jobs and storage](../concepts/jobs.md).

Add `--tui` for a Zellij-style terminal frame, bottom status bar and floating
review panel:

```bash
pvisor run --tui -- bash
```

Keyboard input goes to the shell or Agent by default, and the terminal always
keeps its full width. The header shows the working directory and the file and
network boundaries; the bottom bar shows run state, elapsed time and nonzero
access anomalies, and highlights pending authorization. Detailed counters stay
in the panels.
Press `Ctrl-]` to open the menu, which replaces the same line with shortcuts.
pVisor's own startup diagnostics appear in the
Log panel rather than the Agent terminal. Press `Ctrl-]` to enter command mode,
then `r`, `f`, `n`, `u`, `l` or `p` to open the Overview, Files, Network, Job,
Log or Permissions panel, and `?` for key help. In a panel, switch views with
Tab or `1`–`6`, scroll with `j`/`k`, and return to the Agent with Esc or
`Ctrl-]`. Press `Ctrl-]` twice to send the key to the Agent literally.

### Staging and storage {#暂存与存储}

The table below describes the default CLI behavior. Explicit commit settings,
writable shares and application-compatibility policies can change where writes go.

| Mode or directory | Write destination | After exit |
| --- | --- | --- |
| Ordinary host, no staging | Original workspace | Written to the host; `drop` cannot undo it |
| `--safe` or `--ask` workspace | Copy-on-write stage in Job storage | Retained for `status --review`, `apply` and `drop` |
| Workspace with explicit `--stage PATH` | The named stage | Retained; the option also enables staging |
| VM workspace | The named stage or default Job storage | Retained |
| Other writes to the VM root | Private temporary upper | Discarded when the VM exits |
| `--safe` HOME / `CODEX_HOME` state | Separate private stage | Discarded at exit; not part of the workspace Run Bundle |
| Explicit `--mount SOURCE:write` | Host SOURCE | Written directly and persistently; bypasses workspace apply/drop |

Job records and the Run Bundle are stored in run storage. `--stage PATH` chooses
the location; it is not a prerequisite for `--safe`/`--ask` to retain workspace
changes. File review does not undo remote side effects that already happened.

### Filesystem parameters {#文件系统参数}

A macFUSE temporary workspace on macOS uses the directory where pVisor was
launched as its default lower; `/Volumes/pvisor-*` is the merged-view mount
point and contains the files already present in that directory. An explicit
working directory or OverlayFS base takes precedence.

An ordinary host Job writes workspace changes through to the lower by default.
`--safe` and `--ask` retain workspace changes in Job storage by default, to be
handled later with `status --review`, `apply` or `drop`. `--stage PATH` only
chooses the storage location and is not a prerequisite for retaining
changes.

```bash
pvisor run --stage ../run-stage -- codex
pvisor run --safe --mount /opt/zcode:read --mount /var/lib/zcode:write -- zcode
pvisor run --access '**/.ssh:deny' -- zcode
pvisor run --access '.env:ask' -- codex
```

`--mount SOURCE:read` grants a host executor read-only access at the original
absolute path and requires `--safe` or `--ask`; `SOURCE:write` modifies host
storage directly. Neither rewrites TARGET, and neither may overlap the
workspace, Job storage or writable runtime paths; a read-only share cannot live
under Linux's private `/tmp`. Explicit shares bypass the workspace OverlayFS ask
rules.
`--mount SOURCE[:TARGET]:stage` instead adds SOURCE as a lower layer of the
workspace copy-on-write view; it is not an independent directory mount.
`--access PATH-GLOB:deny|ask|warn` accumulates configuration, preset and CLI
rules by default: deny refuses, ask prompts, and warn allows with a warning.
Clearing configuration and default file protection requires an explicit
`--clear-access` before adding CLI rules. deny > ask > warn.
The former `:read` warning spelling is rejected; use `:warn`, or
`--mount PATH:read` for true read-only access.

Use `pvisor --ask -- bash` to ask before matching an `ask` file rule or reaching
an unlisted proxy network destination; `--ask` also enables `--tui` and
`--safe`. Specifying `--access ...:ask` automatically enables the audit TUI and
the safe staging view, with no separate `--ask` or `--tui`.

The prompt uses Tab or the up/down arrows to switch scope, retention and
buttons, left/right to choose, and Enter to confirm on a button; the deny button
is selected by default. Press `s`, `w` or `u` to pick the session, workspace or
user scope. Press `1` to allow only this file, `2` to allow the containing
directory, or `3` to allow the same suffix; press `d` to deny this target. For
an unlisted proxy network destination, the `--ask` prompt can press `1` to allow
only this target or `2` to allow the current domain and its subdomains; both
choices are limited to the current port and transport, and an IP address cannot
use domain scope. An explicit `deny` rule still refuses directly without opening
the prompt.

Session decisions are written to `audit-policy.json` in the current Job
directory, and workspace and user decisions to the `permissions` section of
`~/.config/pvisor/config.toml` (or the `pvisor/config.toml` under an absolute
`XDG_CONFIG_HOME`). A decision is applied automatically when the same scope
matches again, and every decision is recorded in `audit.jsonl`. The workspace is
identified by its canonical working directory; a new TUI Job loads the rules
with session > workspace > user precedence, and within a layer the last matching
rule wins. Persisted file rules use original absolute paths, so a user-level
suffix rule can cover other workspaces; saving preserves other configuration
settings and comments. Saved decisions are used only for ask-matched access and
cannot override a static deny or an outer sandbox.

In the Permissions panel, select a decision with `j`/`k` and press `x` twice to
remove it; the access then falls back to a broader rule or a new prompt. Removal
does not close already-open file handles, and other running TUIs pick up the
change on their next launch. These records are retained after the Job by
default; `--stage PATH` chooses the location.

Proxy network auditing is a cooperative boundary: direct connections that bypass
the proxy do not trigger this prompt.

```text
Jobs:
  run         Start a Job
  status      Show Job status
  kill        Terminate a Job
  suspend     Save execution state and suspend a Job
  resume      Continue a suspended Job
  fork        Branch from staged files or VM execution state
  checkpoint  Create, list, show, delete, verify, import-base, verify-base, gc


Filesystems:
  inspect     Open a read-only Job filesystem view
  review      Review staged changes and execution evidence
  apply       Accept selected staged changes
  drop        Discard staged changes

Extensions:
  service     Native service lifecycle and installed daemon/cache/memory-pool companions
  replay      Replay an Agent trajectory (when installed)
  tui         Interactive Job terminal (when installed)
```

## Safe first run {#安全的第一次运行}

```bash
pvisor run --safe -- codex
pvisor status --review last
```

Host execution preserves the host filesystem view by default; only
`--filesystem sandbox` enables pVisor's synthetic-root/Landlock or Seatbelt
filesystem access policy. `--safe` stages the workspace and gives HOME
(including Codex launched from a shell) a separate copy-on-write view. Without
`--stage`, the changeset and Run Bundle are retained in Job storage by default
and can be reviewed, applied or dropped after exit. An explicit
`--stage <PATH>` retains the Job and a writable stage, keeps the changes
available for human review, and writes `run-bundle.json` with mode `0600`.

`--strict` requires non-bypassable enforcement evidence for every requested
capability dimension, and otherwise fails closed before the Agent starts. Today
host, container and VM all request Network and Subprocess, and none claims
Subprocess, so `--strict` exits with `UnsupportedPolicy` on those paths. Use the
flag to verify fail-closed behavior, not to mean "a stronger sandbox is ready".
On Linux, `--filesystem sandbox` uses pVisor's rootless launcher with
user/mount/PID namespaces, a minimal bind-projected root, `chroot` and a
kernel-negotiated Landlock policy. `--overlaynet-deny-all` independently adds a
private network namespace; public/allowlist proxy modes remain cooperative. On
macOS, the host executor installs a generated Seatbelt policy only when
filesystem sandbox or network isolation is requested; filesystem policy and
network policy are independent. For a deny-all Run it blocks IP and ambient host
Unix sockets while retaining the exact AgentCtl and Run-local IPC. Reads and
selective network policy remain ambient/cooperative and are labeled separately
in the Bundle. Native OCI and pVisor VM executors retain the same outer Run,
OverlayFS and AgentCtl state observation.

After completion:

```bash
pvisor status --review last
pvisor fork last -- codex
pvisor apply last --all # or: pvisor drop last
```

`fork` snapshots the stopped Job's staged filesystem before launching the child.
Pass `--checkpoint ID` to reuse an existing logical checkpoint. Embedded hosts
can call `RunHandle::checkpoint`: pVisor publishes an AgentCtl quiesce
directive, requires every Session frozen into the checkpoint to report the
matching quiesced state, snapshots the raw upper, then publishes `continue`.
Logical checkpoints preserve filesystem and cooperative client safe-point
boundaries, not process memory.

To stop a running Job, use `pvisor kill JOB_ID`. It requests graceful termination
from the Job supervisor; check `pvisor status JOB_ID` for the final state. A
stopped Job can still be reviewed and applied or dropped.

## Job checkpoint management

`run` retains its existing options and defaults. These interfaces support workspace checkpoints for stopped Jobs:

```bash
pvisor checkpoint create ./stage/task --request-id before-refactor --json
pvisor checkpoint list ./stage/task --json
pvisor checkpoint show ./stage/task CHECKPOINT_ID --json
pvisor review ./stage/task --checkpoint CHECKPOINT_ID --diff
pvisor inspect ./stage/task --checkpoint CHECKPOINT_ID -- ls
pvisor fork ./stage/task --state workspace --checkpoint CHECKPOINT_ID --stage ./stage/branch -- codex
pvisor checkpoint delete ./stage/task CHECKPOINT_ID --json
pvisor checkpoint gc ./stage/task --json
```

Checkpoints belong to the selected Job. The default kind is workspace, preserving
the staged upper, preimages, policy and source Attempt. Lower layers remain external
path references; this does not save process memory or guarantee that external
lowers remain unchanged. Unique ID prefixes resolve; ambiguous prefixes fail.
Creation requests can be retried with `--request-id`; a deleted result cannot be
captured again by reusing the same key. Forks retain hard-link references to the
source manifest, requiring parent and child stages on the same filesystem;
deletion is refused while references exist. `drop JOB` preserves the Job,
checkpoints and branch references. `apply/drop` require an explicit Job and records
confirming it has stopped. `review` JSON distinguishes historical execution
evidence from the currently selected file view; successful apply/drop advances
the workspace generation.

Reading or forking historical workspace checkpoints still requires the source Job lease and refuses a running source.

### Full VM execution checkpoints {#full-vm-execution-checkpoints}

For VMs with native capture support, these commands seal CPU, RAM, devices and filesystem state:

```bash
pvisor run --executor vm --rootfs /path/to/rootfs --overlaynet off --stage ./stage/task -- /bin/agent
pvisor checkpoint create ./stage/task --kind execution --ram-storage compressed --request-id save-1 --json
pvisor suspend ./stage/task --ram-storage raw --request-id pause-1 --timeout 2m --json
pvisor resume ./stage/task --request-id resume-1
pvisor fork ./stage/task --state execution --checkpoint CHECKPOINT_ID --stage ./stage/branch --request-id branch-1
pvisor checkpoint verify ./stage/task CHECKPOINT_ID --json
pvisor checkpoint gc ./stage/task --kind execution --json
```

`create --kind execution` captures and continues the source. `suspend` succeeds only after publication and confirmed native VM termination; timeout only ends the client's wait and is not termination evidence. Retrying the same `--request-id` does not repeat capture. `resume` restores only the current suspended head, retaining the Job ID and creating a new Attempt while preserving previous records and Bundles. The original stage remains a selector for the current Attempt. Restoration retains the captured guest environment instead of inheriting the shell issuing resume.

Execution forks reject replacement commands. Selecting a historical checkpoint can leave the parent running; without one, a running parent is captured and continued, while a suspended parent supplies its head. The child owns private RAM and filesystem upper layers. `--ram-storage raw|compressed` applies only to new capture and defaults to compressed. `resume` and execution `fork` accept `--eager-ram` to read all RAM before startup; omission keeps lazy loading.

The current native profile supports Linux x86_64 and macOS ARM64 with no network devices and private RAM. Host root `/`, networking devices, shared memory pools, writable RAM backing and cold-page compression are outside this restore contract. `run` does not disable networking or DAX or change rootfs to enable capture; `status JOB --json` reports capability and blockers. Restore requires the same host boot, pVisor binary and firmware; cross-host and cross-version restore are unsupported.

When the stage is inside the workspace, capture storage is placed outside guest backing roots and recorded as Job-owned. When necessary, restored file copies also use an independent directory, with their paths retained in the Attempt record for review/apply. Capture retains the guest-visible projection, excluding stage management directories already hidden from the guest. Visible content, metadata and hard-link audits remain complete.

Suspended Jobs refuse apply/drop and workspace capture. `kill JOB` withdraws continuation rights while retaining checkpoint history, allowing subsequent workspace decisions. Execution checkpoints share list/show/delete entries with workspace checkpoints; deletion checks the suspended head, branch references and storage leases. Branch references are retained conservatively; Job deletion/archiving has no release interface yet. GC collects unpublished transactions, tombstones and unreferenced RAM content in this Job's stores, never published checkpoints. It is not cross-Job store-wide collection and does not clean daemon sandbox state.

Immutable base management uses `checkpoint import-base JOB ROOTFS --json` and `checkpoint verify-base JOB BASE_ID --json`. Import returns an owned rootfs path usable by subsequent ordinary `run --rootfs`. The CLI exposes snapshots through Job commands, not a standalone snapshot frontend; old stores are not automatically converted into Job checkpoints. See [Job checkpoint design](../design/job-checkpoint-cli.md#10-当前实现与验收边界) for implementation and acceptance.

## `--safe` parameter preset {#safe-参数预设}

```bash
pvisor run --safe -- claude
pvisor run --safe --vm --rootfs image=my-agent-image:latest -- codex
pvisor run --safe -- zcode
pvisor run --safe --overlaynet-allow inference.example.com:443 -- zcode
```

`--safe` generates a command-line argument patch, applies it through the same
CLI parser, and then applies explicit user arguments. The preset matches agents
by executable name and automatically grants the corresponding API's HTTPS
destinations. `--safe` requires the selected executor to enforce isolation; it
never selects an executor. Precedence is **explicit CLI > safe preset >
configuration file > ordinary defaults**. It supports ordinary commands and
TOML `--config`; a prepared JSON `--spec` rejects the preset.

`--safe` directly requires file read, file write and network isolation
enforcement, and does not allow a silent fallback to an ordinary host process.
It adds no separate sandbox command-line option or configuration setting.
`--strict` remains a check over all requested capabilities, including resource
limits, and is distinct from this isolation requirement.

- macOS host: Seatbelt enforces read/write scope and allows only connection to
  pVisor's allocated loopback TCP proxy port; it blocks other direct IP egress
  and ambient host Unix sockets, keeping only the Run-local IPC that is
  required. The Agent uses a temporary HOME and cannot read the original home
  directory directly; credentials must be passed explicitly or held by Gateway.
  System runtime libraries and path metadata needed for startup remain readable.
- Linux host: `--safe` requires rootless namespaces, a synthetic root, chroot,
  Landlock and a copy-on-write HOME view. Selective egress and Gateway traffic
  are forwarded cooperatively through the supervisor loopback proxy, and direct
  sockets may still bypass it; use a VM or deny-all when a non-bypassable network
  boundary is required. The launcher projects the host HOME through a private
  stage; overlay deny rules do not hide secret files at their original paths
  outside the workspace view.
- VM: requires the existing `auto` network boundary; safe does not select a VM
  automatically.
- container: currently lacks a complete enforcement boundary, so `--safe`
  refuses to start.

Isolation setup failure stops the run. `--safe` cannot be combined with
`--overlaynet off`.

The preset grants only the HTTPS destinations (port 443) in the table below;
other destinations are denied by default.

| Command | Default destinations |
|---|---|
| `codex`, `bash`, `sh`, `zsh`, `fish` | `api.openai.com`, `chatgpt.com`, `ab.chatgpt.com` |
| `claude` | `api.anthropic.com` |
| `gemini` | `generativelanguage.googleapis.com` |
| `zcode` | `api.z.ai`, `open.bigmodel.cn` |

Unknown commands deny ordinary egress. Set grants explicitly with
`--overlaynet-allow HOST:PORT` (which replaces the preset list); existing deny
rules and rate limits remain in force, and Gateway capture uses explicitly
configured routes.

Without `--safe`, Codex state and project writes reach the host lower. With
`--safe`, the workspace uses the reviewable Run stage and HOME (and an explicitly
set `CODEX_HOME`) uses a separate private stage. HOME state changes are discarded
when the Run ends and are not part of the workspace Run Bundle.

Independently of `--safe`, running `zcode` directly on a Linux rootless host
applies a compatibility policy: it inherits the host environment and allows
direct persistent writes to existing `~/.zcode`, `$XDG_CONFIG_HOME` (or
`~/.config`) and `~/.local/share/applications`. When a bundled Chromium setuid
helper is found, pVisor hides it and adds `--no-sandbox`; it also adds
`--disable-gpu`. This path disables Chromium's own sandbox while retaining
pVisor's outer rootless boundary. These application-state writes bypass the
workspace stage, even with `--safe`. The policy matches the direct executable
name, so shell wrappers do not trigger it. The `zcode-bigmodel` Gateway profile
is a separate routing adapter.

The preset uses `--clear-pass-env` to clear configured `run.pass_env`;
`--clear-pass-env` also works on its own, and an explicit `--pass-env NAME`
afterward still applies. Direct Codex commands keep host environment inheritance
to preserve account and routing configuration; other commands disable
inheritance by default and receive credentials through an explicit
`--pass-env NAME`. Explicit CLI options can re-grant or override this. `--safe`
stages the workspace by default; existing container mounts and filesystem lower
layers are retained, and the project base, rootfs and executor are unchanged.
Deliver credentials to the Agent with an explicit `--pass-env`, or let a
configured Gateway hold the upstream key on the trusted side.

Startup prints the effective policy and override notices. Note that:

- Without `--safe`, host/container selective proxies remain bypassable.
- Hostname rules cannot distinguish inference, telemetry and upload APIs under
  the same hostname, or prevent content smuggled inside model requests.
- Wildcard rules cover the OverlayFS view; on Linux the projected HOME may still
  expose sensitive files at their original paths, and explicitly granted extra
  paths need their own protection. Filename rules cannot recognize renamed
  copies, secrets in source code or content in Git history, and there is no
  bulk-read or tool-call attribution monitoring.
- Expanding the share scope or choosing `--rootfs host` increases the accessible
  data; `--safe` cannot be combined with disabling OverlayNet.

## File access rules

```bash
pvisor run --safe --mount /opt/tool:read --access '**/.ssh:deny' --access '**/.env*:warn' -- my-agent
```

`--mount SOURCE:read|write` is a host executor's explicit share, granting
read-only or persistent write access respectively; a read-only share requires
`--safe` or `--ask`. `--mount SOURCE[:TARGET]:stage` composes a lower layer of
the workspace view.
`--access PATH-GLOB:deny|ask|warn` appends file rules and does not replace the
default protections; only `--clear-access` clears them explicitly.
`warn` only warns and is not read-only; file approval covers inspection,
reading, modification and deletion within the view.

Rules match relative to the mount root: `*` does not cross directories, `**` may
cross directories, and matching a directory covers all of its descendants.
To prevent alias bypasses on case-insensitive filesystems, matching is
case-insensitive; absolute paths, empty rules and `.`/`..` path components are
invalid. deny takes precedence over warn. A path that matches deny is hidden
from directory enumeration, and access, creation and modification are refused;
warn allows access and prints the path to the supervisor's stderr without
printing file contents. A warning indicates a filesystem access attempt
(including metadata access), not an exact count of content reads; kernel caching
may coalesce accesses.

The default `--safe` deny covers `.ssh` and `.gnupg` directories at any level
and the files `id_rsa`, `id_dsa`, `id_ecdsa`, `id_ecdsa_sk`, `id_ed25519` and
`id_ed25519_sk`. The default warn covers `.env`, `.env.*`, `*.pem`, `*.key`,
`*.pub`, `*.p12`, `*.pfx`, `.aws/credentials`, `.netrc` and `.npmrc`. Public
keys inside `.ssh` are hidden with the directory; public keys outside only warn.
This does not identify every private key; add rules for custom file names.

In TOML:

```toml
[overlayfs]
stage = "../stage-001"
mount = [{ source = "/opt/tool", access = "stage" }]
access = [
  { path = "**/.ssh", level = "deny" },
  { path = "**/.env", level = "warn" },
]
```

Explicit CLI access rules are appended after configuration and safe presets;
deny takes precedence over warn. Rules are stored with the Run and overlay
records.

FUSE and VM virtio-fs share the rule checks. With deny enabled, this version
conservatively refuses all multiply-linked regular files and new hard links to
prevent alias reads; ordinary directory renames, exchanges and deletions check
the affected subtree and refuse the operation when it includes a protected file.
Symlinks are resolved by the mount namespace, and file opens do not follow a
final symlink to the raw backing file. The VM applies rules to the root view as
well and protects the workspace's original paths and the overlay backing
directories. This does not replace executor isolation: host environment reads,
extra container shares and credentials outside this view still require separate
controls. Backing directories should be managed by a trusted supervisor; the
rules do not promise to resist races from other host processes concurrently
rewriting backing files.

## Replay an Agent trajectory {#replay-an-agent-trajectory}

`pvisor replay` assumes the caller has normally created a fresh sandbox. It
replays complete tool batches through `after_step`, rebuilds the selected
Agent native context with fresh observations, then starts the live Agent:

```bash
pvisor replay \
  --agent claude-code \
  --trajectory /input/session.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/claude \
  --boundary-user-prompt 'Review the fresh observation before continuing.'
```

OpenHands, mini-swe-agent, Pi agent, OpenCode, Codex and SWE-agent use the model
endpoint and credentials already present in their environment. Pi requires the
exact `0.83.0` runtime and accepts native RPC event JSONL containing the core
`read`, `bash`, `edit` and `write` tools. Claude Code uses a temporary bridge
owned by SandboxReplay because its native resume transport inserts wake-up
messages. The bridge validates and removes that exact Resume Transport envelope
before forwarding the model request. It does not enable pVisor Gateway, capture
model traffic or persist a bridge audit.

OpenCode requires the exact `1.17.7` runtime and its native
`opencode run --format=json` event JSONL; Codex requires the exact `0.149.0`
runtime and Codex rollout `response_item` JSONL. Both rebuild the native prefix
in the fresh sandbox and invoke their native resume command for continuation.
Codex derives its native session ID from the trajectory's `session_meta`; the
request `session_id` is only a model-router/Run key and cannot override the
native session. When the native session is missing, continuation fails closed.

The equivalent strict replay TOML is:

```toml
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 30
agent_entrypoint = "/usr/bin/claude"
max_steps = 200
session_id = "task-291-attempt-1"
replay_only = false
disable_thinking = true
boundary_user_prompt = "Review the fresh observation before continuing."
```

Pi uses the same CLI/TOML surface. When the runtime is installed at
`/opt/pi-agent`, for example:

```bash
pvisor replay --agent pi-agent \
  --trajectory /input/pi-agent.events.jsonl \
  --after-step 30 \
  --agent-entrypoint /opt/pi-agent/bin/pi
```

Replay has three modes. The default replays the prefix and continues;
`--replay-only` executes the prefix and stops before a model request; and
`--prepare-only` constructs the prefix without executing tools or requiring a
runtime. `--max-steps` is the total action budget, including replayed actions.
`--allow-stale-observations` is an explicit Claude-only escape hatch that marks
the v3 result `degraded`.

`--boundary-user-prompt TEXT` appends one user message after the final fresh
observation and before the first live model inference. The TOML spelling is
`replay.boundary_user_prompt`. It is ignored for inference in prepare-only and
replay-only modes, and an omitted option preserves the unmodified replay
boundary. Structured results and replay journals store only injection state,
length and a digest; Agent-native prepared or continued trajectories may
contain the user message.

The result schema is `sandbox-playback.result/v3`, with typed `phase`, `quality`
and `agent_status` fields plus state/output locations, artifacts and an optional
structured failure. Non-Claude callers that used `replay_only = true` only to
construct a prefix must migrate to `prepare_only = true`.

`disable_thinking` belongs to `[replay]` and is also exposed as
`--disable-thinking`. Claude Code's protocol bridge applies it to the upstream
request; OpenCode omits its `--thinking` flag when it is set. It does not turn
on Gateway capture. Optional `[run]`, `[overlayfs]` and `[overlaynet]` sections
create an outer managed `pvisor run`; they do not change the inner replay model
path.

By default, replay's internal state, WAL, manifest, fresh-observation
comparisons and native working files remain under
`/tmp/pvisor-sandbox-replay` and disappear with the sandbox. Replay does not
enable pVisor Gateway, a model-traffic capture store or a Claude Resume
Transport audit. A caller that explicitly selects `--state-dir` or
`--output-dir` owns those files. Use `--replay-only` to execute the prefix and
stop before live inference, or `--prepare-only` to construct it without
execution.

## One configuration model

`pvisor run` has one canonical `RunConfig`. The CLI covers its common fields,
but `run.inherit_env` currently has no direct CLI switch. Moreover,
`apply_safe_defaults` currently clears environment inheritance for non-Codex
CLI commands even without `--safe`; the ZCode host adapter re-enables it for
direct `zcode`. Treat TOML `inherit_env` as ineffective on those CLI paths.
`--config` reads an explicit TOML `RunConfig`; `--spec` requires a prepared
JSON `RunSpec` for delegated execution and cannot be combined with other Run
overrides. pVisor does not discover a hidden project configuration file.

```bash
pvisor run \
  --name my-agent \
  --stage ../stage-001 \
  --mount /opt/tool:stage \
  --access '**/.ssh:deny' \
  --overlaynet-allow api.openai.com:443 \
  --overlaynet-deny 169.254.0.0/16 \
  --overlaynet-limit 10mbps \
  --gateway-mode capture \
  --gateway-level dialogue \
  --gateway-route \
    'name="openai", provider="openai", upstream="https://api.openai.com/v1", api_key_env="OPENAI_API_KEY"' \
  --record-destination ./capture \
  -- my-agent
```

`--record-destination` writes a local `pvisor_core::event::Event` Journal to
`events.trace.jsonl`. Legacy JSONL is not supported. Journal positions define
append order and `caused_by` defines causal links; `observed_at_unix_ms` is
observation metadata, not an ordering source of truth.

The equivalent TOML is:

```toml
# host（默认）或 sandbox；与 OverlayNet 和 OverlayFS 暂存相互独立
filesystem = "host"

[run]
agent = "my-agent"
executor = "host"
command = ["my-agent"]

[overlayfs]
stage = "../stage-001"
mount = [{ source = "/opt/tool", access = "stage" }]
access = [{ path = "**/.ssh", level = "deny" }]

[overlaynet]
mode = "proxy"
policy = "allowlist"

[[overlaynet.rules]]
host = "api.openai.com"
ports = [443]

[[overlaynet.deny]]
host = "169.254.0.0/16"

[[overlaynet.limits]]
bytes_per_second = 1250000

[gateway]
mode = "capture"
level = "dialogue"

[[gateway.routes]]
name = "openai"
provider = "openai"
upstream = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"

[record]
destination = "./capture"
```

Run it with `pvisor run --config run.toml`. Explicit CLI scalars replace TOML
scalars. Network and Gateway list options replace their complete configured
lists; filesystem `--mount` and `--access` entries are appended to configured
entries. The serialized `[overlayfs]` fields `stage`, `mount`, `access` and
`max_size` map to `--stage`, `--mount`, `--access` and
`--overlayfs-max-size`. The command after `--` replaces `run.command`.
The size limit is checked after execution, so it does not bound peak space used
while the Agent is running.

`--container-image IMAGE` selects the native OCI container executor
automatically; `--executor container` makes the choice explicit. The transport
generates a standard OCI bundle, uses the current or explicitly supplied
compatible Linux pVisor, mounts it into the rootfs, sets the process
args, and takes the normal `pvisor run --executor host --spec ...` path. The
Agent command is carried inside the RunSpec rather than exposed in OCI runner
argv. The injected pVisor creates its own AgentCtl and returns a typed
RunResult. The final OverlayFS cwd and session Gateway configuration are mounted
at stable paths.
`--container-rootfs PATH` supplies an existing rootfs directly; otherwise pVisor
prepares `--container-image` from its bundled OCI image store. The runtime must
be `runc` or `crun`; pVisor does not invoke Docker/Podman. User mounts are
repeatable TOML inline tables, for example:

```bash
pvisor run \
  --container-image example/codex-agent:latest \
  --container-pvisor-binary ./dist/pvisor-linux-amd64 \
  --container-platform linux/amd64 \
  --container-network none \
  --container-mount \
    'source="/host/cache", target="/cache", read_only=false' \
  -- codex
```

The example above assumes Linux x86_64. `--container-platform linux/amd64`
or `linux/arm64` asserts the native host architecture; a matching value is
accepted and a cross-architecture value is rejected, even with a prepared
`--container-rootfs`. Host and VM configurations reject this option. It does
not select emulation or discover/download an executable. The injected binary
defaults to the running pVisor; use `--container-pvisor-binary` for an explicitly
provisioned Linux build compatible with the native architecture and rootfs ABI.
See [container setup](../guides/executors/container.md) and
[configuration values](config.md#settings).

The in-process Gateway and explicit OverlayNet proxy currently require
`container.network = "host"`, because their injected addresses are host loopback
endpoints. `none` mode is valid when these drivers are off; `bridge` requires
external CNI configuration and is currently rejected. The executor records
container isolation but does not claim complete capability enforcement.

`--executor vm` uses statically linked `pvisor-vm` and its embedded init to boot a
minimal Linux guest. `--vm-ram-backing FILE` (`[vm].ram_backing`) creates a new
private file backing live RAM. In ordinary file-backed mode, omitting it creates
an attempt-local file in the user cache, deleted on normal exit; the cold pager
below instead uses anonymous RAM without a live backing file. `--vm-ram-compression`
(`[vm].ram_compression = true`) selects the PVZRAM v2 manifest and immutable
Zstd Seekable base/delta sidecar files
at startup, requiring Linux FUSE or the macFUSE kernel backend.
Use the live VM options below or Rust `RunHandle::pause/resume/offload` to control
the live VM. A new offload destination must be on the backing's existing filesystem.
Reclaim reports sampled residency, not guaranteed zero RAM. The file is not a
complete VM snapshot. Existing [storage/control tests and compressed artifacts](../design/offload/index.md#experiments)
establish limited implementation evidence, not end-to-end guest correctness or
production memory savings. Compressed exit still does not commit a new generation:
writes after the last resume may be discarded, leaving only the last committed head.

### VM memory and control options {#vm-memory-options}

| Run option | TOML field under `[vm]` | Default / purpose |
| --- | --- | --- |
| `--vm-control-socket PATH` | `control_socket` | Unset: CLI creates `/tmp/pvisor-host-<effective-UID>/vm-<UUID>.sock` under canonical `/tmp` |
| `--vm-ram-backing FILE` | `ram_backing` | Unset: attempt-local backing in ordinary file-backed mode; new files only |
| `--vm-ram-compression[=BOOL]` | `ram_compression` | `false`; FUSE/macFUSE Seekable backing |
| `--vm-cold-ram-compression[=BOOL]` | `cold_ram_compression` | `false`; Linux x86_64 local live cold pager |
| `--vm-ram-dedup[=BOOL]` | `ram_dedup` | `false`; best-effort host dedup advice |
| `--vm-memory-pool SOCKET` | `memory_pool` | Unset: experimental external pool; Linux shares physical pages without userfaultfd |
| `--vm-node-socket SOCKET` | `node_socket` | Unset: same-host node resource service for immutable images/restored RAM |
| `--vm-snapshot-filesystem-pool DIR` | `snapshot_filesystem_pool` | Unset: owned copies; optional host-owned immutable lower pool for Linux x86_64 no-network native checkpoints |

The three boolean options accept a bare flag (meaning `true`), `=true` or
`=false`; a separate `false` argument is not the boolean grammar. Omission
preserves the configured value, including `true`. For example, override dedup
without clearing other settings:

```bash
pvisor run --config run.toml --executor vm --vm-ram-dedup=false -- /bin/sleep 600
```

A true boolean or one of these explicit path options selects VM execution when
`--executor` is omitted; `=false` alone does not select VM. An explicit executor
is not silently replaced. Non-VM execution does not support live VM controls;
a configured control socket with an explicit host/container executor is rejected.
Path options replace only their matching field and leave omitted paths intact.

The local cold pager combines reclaim and instance-local compression in one
option; there are no separate cold-reclaim and local-compression toggles. It
conflicts with backing/FUSE compression, dedup, external pools, snapshot
capture/restore, the snapshot filesystem pool and whole-VM offload. Dedup
conflicts with either compression mode and external pools. These conflicts are
checked after config/CLI merging, so use explicit `=false` overrides as needed.
The filesystem pool must be host-owned, outside VM-writable roots and snapshot
stores, and on the Job's volume; setting a node socket does not grant transparent
recovery if the service fails.

### Live VM Attempt controls {#vm-instance-control}

Every native VM Attempt gets a host-only control endpoint automatically,
including embedded runs without retained Job storage. CLI endpoints live directly
under the private Host service root. `pvisor run` prints its exact identity to
**stderr**:

```text
pVisor live VM options: --vm-socket /tmp/pvisor-host-1000/vm-EXAMPLE.sock --vm-job-id run-EXAMPLE --vm-attempt-id attempt-EXAMPLE (status; suspend JOB --vm-pause/--vm-offload; resume JOB --vm-load)
```

Copy the socket, Job ID (the internal Run ID) and Attempt ID from that line into another host terminal;
the following example values must be replaced with that live identity:

```bash
pvisor status --vm-socket /tmp/pvisor-host-1000/vm-EXAMPLE.sock --vm-job-id run-EXAMPLE --vm-attempt-id attempt-EXAMPLE
pvisor suspend run-EXAMPLE --vm-socket /tmp/pvisor-host-1000/vm-EXAMPLE.sock --vm-job-id run-EXAMPLE --vm-attempt-id attempt-EXAMPLE --vm-pause
pvisor resume run-EXAMPLE --vm-socket /tmp/pvisor-host-1000/vm-EXAMPLE.sock --vm-job-id run-EXAMPLE --vm-attempt-id attempt-EXAMPLE --vm-load
pvisor suspend run-EXAMPLE --vm-socket /tmp/pvisor-host-1000/vm-EXAMPLE.sock --vm-job-id run-EXAMPLE --vm-attempt-id attempt-EXAMPLE --vm-offload --vm-ram-file /private/vm-ram/offloaded.ram
pvisor resume run-EXAMPLE --vm-socket /tmp/pvisor-host-1000/vm-EXAMPLE.sock --vm-job-id run-EXAMPLE --vm-attempt-id attempt-EXAMPLE --vm-load
```

The global options `--vm-socket PATH`,
`--vm-job-id ID` and `--vm-attempt-id ID` are required together, even for
live `status`; stale or mismatched identities are rejected. `suspend` and
`resume` require a positional Job selector matching `--vm-job-id`, not `last`
or a stage path. Only `status`, `suspend --vm-pause` / `--vm-offload`, and
`resume --vm-load` accept this live control mode. Pause and offload are mutually
exclusive. These flags do not change ordinary persisted-Job suspend/resume:
without them, those commands use execution checkpoint capture/restoration.

`--vm-offload` accepts optional `--vm-ram-file PATH`: omit it to use the existing
backing, or choose a new, guest-inaccessible path on the same filesystem. Wait
for success before using the published file. `--vm-load` selects
`HostVmCommand::Resume`, mapped to `RunResume`; there is no `Load` wire operation.
It reloads/unpauses the same live Attempt without eager RAM prefaulting. It
neither restarts a process nor restores a persistent snapshot. `--vm-pause`
stops vCPUs, not the stronger CPU/device quiescence used by offload.

Successful live control replies are JSON on stdout containing `HostVmResult`
fields `status` and `value`. Core's `host_protocol` owns `HostVmCommand` and
`HostVmResult`; `pvisor::host_vm_exchange` exchanges
`AgentCtlHostRequest<HostVmCommand>` / `AgentCtlHostResponse<HostVmResult>` for
embedded callers. The endpoint
wire wraps results in version-1 Host envelopes with a correlated `request_id`;
typed Host errors are nonzero CLI failures, not necessarily JSON on stdout. Transport/connect and parsing failures also exit
nonzero. Non-VM controls are explicitly unsupported, not a process-signal
fallback. The endpoint is removed when the Attempt ends and is separate from
the staged Job's control discovery link.

To choose a stable CLI path, use the private service root. This Linux example
assumes effective UID `1000`; replace the root with your effective UID and
canonical temporary path (usually `/private/tmp` on macOS):

```bash
install -d -m 0700 /tmp/pvisor-host-1000
pvisor run --executor vm --vm-control-socket /tmp/pvisor-host-1000/ctrl.sock -- /bin/sleep 600
```

CLI custom sockets must be directly in the canonical private Host service
root; arbitrary private parents are rejected. The root must be a non-symlink
directory owned by the effective UID with mode exactly `0700`; existing socket
paths are never overwritten. The socket has mode `0600` and accepts only
same-UID peers. `--vm-control-socket` chooses a path when creating a VM;
`--vm-socket` addresses an existing live VM. Embedded callers have a separate
custom-parent API with private-parent validation. Host authority endpoints are
excluded from guest access, including host-rootfs VMs; do not expose the service
root through guest mounts or writable roots. These are not guest discovery files.

### VM local live cold compression {#vm-cold-ram-compression}

`--vm-cold-ram-compression` sets `[vm].cold_ram_compression = true` and selects
VM execution on Linux x86_64. The default is `false`; omitting the flag preserves
a configured value. The runner automatically starts an experimental userfaultfd
pager over private anonymous RAM with bounded instance-local `LocalColdRamStore`.
It needs neither FUSE nor a pool and is not `--vm-ram-compression`.

Kernel-fault userfaultfd syscall or `/dev/userfaultfd` permission is required;
compiled support is not authorization. Missing permission fails startup without
fallback, and pVisor changes no global sysctl. See [local compression](../design/memory-optimization/compression-local.md#direction)
for a user-specific ACL grant/revoke example and restricted mappings/build features.
It rejects `vm.ram_backing`, `vm.ram_compression`, `vm.ram_dedup`,
`vm.snapshot_filesystem_pool`, snapshot capture/restore and whole-VM offload.
Linux external `vm.memory_pool` maps physical pool pages privately: reads
retain sharing and writes use COW, without userfaultfd. It requires private anonymous RAM and is mutually
exclusive with instance-local compression. Guest execution continues between short capture/recheck windows
without application participation: this is eviction/refault probing, not ordinary
pause or a true read-access heat detector. No production-density gain is promised.

### VM RAM dedup advice {#vm-ram-dedup}

`--vm-ram-dedup` sets `[vm].ram_dedup = true` and selects the VM executor. The default is `false`; omitting the flag preserves a configured value. This is an explicit opt-in to cross-workload content-sharing risks, not a promise of savings. It cannot be combined with `--vm-memory-pool` / `vm.memory_pool`, `--vm-ram-compression` / `vm.ram_compression`, `--vm-cold-ram-compression` / `vm.cold_ram_compression`, or `PVISOR_EXPERIMENTAL_MEMORY_POOL`.

On Linux, a fresh VM with dedup enabled and no explicit `vm.ram_backing` uses private anonymous RAM eligible for KSM. This path does not support whole-VM offload. Explicit writable RAM backing keeps shared mappings and skips dedup advice.

The runner calls `handle.advise_ram_dedup()` explicitly and writes a best-effort installation report to stderr; advice failure does not stop execution. Linux advice covers ordinary private anonymous RAM and restored private COW mappings. Live `MAP_SHARED` RAM is skipped without mapping conversion; macOS reports unsupported for otherwise eligible mappings. `accepted_bytes` means advice was accepted for those ranges, not merged bytes, savings or an enabled KSM scanner. No global KSM settings change and no new service is required. See [current integration and evidence](../design/memory-optimization/deduplication.md#direction) for eligibility, snapshot ownership and validation limits.

### VM rootfs and executor boundaries {#vm-rootfs}

`--rootfs image=IMAGE` selects this executor and pulls an
OCI/Docker image directly, without invoking Docker, Podman or Buildah. When no
explicit rootfs or image is supplied, Linux uses the host `/` through virtiofs
and OverlayFS by default, preserving the host runtime, PATH and HOME without
pulling an image. On macOS, supply a Linux rootfs or image explicitly.
Manifests and layer digests are verified, the host architecture selects
`linux/arm64` or `linux/amd64`, and the unpacked rootfs becomes the immutable
lower layer of a pVisor OverlayFS. `--image-store` overrides the platform cache
directory. OCI cache targets are marked immutable, and this protection survives
logical checkpoint/fork, so `pvisor apply` cannot mutate a rootfs shared by
other Runs.

On Linux, `--rootfs host` selects the host `/` as the VM rootfs lower and
selects the VM executor when `--executor` is omitted. `--rootfs <PATH>` uses a
prepared directory and `--rootfs image=<PATH>` uses an OCI image or image path;
the three are mutually exclusive, and host rootfs is rejected on macOS. This is
the unified rootfs syntax; `--mount SOURCE[:TARGET]:stage` composes extra lower
layers for the guest workspace, and the current workspace is the implicit bottom
layer. Read/write explicit host shares are currently supported only by the host
executor. Workspace changes enter the named stage or default Job storage and are
retained after exit; other writes to the VM root use a temporary upper and are
discarded when the VM exits.

The merged rootfs is guest `/`, and `/workspace` becomes the guest cwd. On both
Linux and macOS, `pvisor-vm` serves pVisor's rootfs and workspace
copy-on-write unions directly over virtio-fs. The VMM never re-exports a host
FUSE mount and does not materialize or reconcile either tree. Linux uses KVM and
Apple Silicon macOS uses HVF through the same executor. Linux static musl builds
embed the guest kernel and require no firmware shared library at runtime, and
they reject `--vm-library-dir`. macOS wheels install `libkrunfw.5.dylib` beside
pVisor; macOS source runs otherwise download the pinned official release into a
SHA-256-verified platform cache, where `/usr/bin/cc` turns the prebuilt kernel
bundle into the required dylib. On macOS, `--vm-library-dir` selects an existing
firmware directory. OverlayNet `auto` uses the non-bypassable VM smoltcp IPv4
TCP/DNS driver, while Gateway capture uses an internal route through the guest
virtual router. Linux additionally confines the VMM with namespaces and
Landlock. The macOS VMM still has the invoking user's host permissions, so
despite the guest-kernel isolation the first OCI-image version must not be
treated as a hostile multi-tenant boundary.

On host/container execution, the four visible OverlayNet policy flags and
Gateway capture automatically enable the proxy driver. `--safe` stages the
workspace by default and `--mount` adds explicit lower layers; see
[Staging and storage](#暂存与存储) for write destinations. When a stage is nested
inside a base or compose layer, pVisor hides that subtree from the merged view
and rejects guest attempts to recreate it. VM Runs create no live host
mountpoint, preventing host indexers from recursively entering `<stage>/merged`.
The reverse topology, where a stage contains a lower layer, is rejected. Until
pVisor can safely materialize a complete merged-vs-base diff, a composed Run
rejects a subsequent `pvisor apply`. Selective host/container network rules
apply to traffic through the explicit proxy. Host deny-all uses a namespace or
Seatbelt to block direct egress; containers can use `--container-network none`
for offline execution. On a pVisor VM, `auto` uses smoltcp IPv4 TCP/DNS and
`off` leaves the guest offline; deny-all still permits configured internal
Gateway routes. See [network boundaries](../guides/policies/network.md) for the
scope of each path.

## Run project discovery {#run-项目发现}

The current directory is the default project association. `--mount` identifies
additional host lower layers and, optionally, their Agent-visible paths. Each
Run receives an independent directory under pVisor's default records root. If
that root would fall inside the selected OverlayFS base or a compose layer,
pVisor instead uses the system temporary Run root to keep the writable stage
disjoint:

```text
project/                         # reusable workspace / default base

~/.pvisor/runs/
└── run-<uuid>/                  # one generated Run and default stage
    ├── run.json
    ├── run-bundle.json          # mode 0600; outcome + safety + changes + effects
    ├── overlay.json             # when OverlayFS is enabled
    ├── upper/
    ├── merged/
    ├── checkpoints/
    ├── lease.lock
    ├── control.sock             # while a live OverlayFS Run is available
    ├── .capture/                # when OverlayNet/Gateway is enabled
    └── events.jsonl             # when --record-destination is set
```

Lifecycle commands accept a Run id, Run directory, project workspace,
`run.json`, upper, or merged path. A project workspace selects its latest Run:

```bash
pvisor status /path/to/project
pvisor inspect /path/to/project -- rg TODO .
pvisor apply /path/to/project --all
pvisor apply /path/to/project --path src --path tests/unit
pvisor apply /path/to/project --include 'docs/**' --exclude 'docs/generated/**'
pvisor apply /path/to/project --target /path/to/another-target --all
pvisor drop /path/to/project
```

`inspect` creates a separate kernel-read-only view. `apply` and `drop` refuse
to mutate a live Run. A filtered apply is dependency-closed and repeatable:
unselected paths remain staged, while opaque directories and hard-link groups
remain atomic. Each successful batch is persisted in `apply-ledger.json`.
The overlay records a durable first-touch fingerprint for every mutated target
path. `apply` fails closed if a selected target path changed after staging;
prepared batches recover forward, and individual non-directory replacements
commit with a same-directory atomic rename. The host filesystem still provides
no single atomic commit point for an arbitrary multi-file batch.
Applying all remaining changes or dropping the stage is terminal; `drop` cannot
undo already applied batches, and `apply` cannot recover discarded changes.
Terminal cleanup removes `upper`, `work` and other disposable staging data but
retains compact Run/Overlay metadata, the apply ledger and capture artifacts.

### Shared image file cache

`pvisor service cache serve` runs the OCI file service in the foreground;
`cache prepare IMAGE`, `cache list DIGEST [PATH]`, `cache stat DIGEST PATH` and
`cache read DIGEST PATH` reach it through `PVISOR_CACHE_SERVER`. The default is
the `pvisor/cache.sock` Unix socket under the user cache directory. The server
accepts `--image-store DIR` to name an existing OCI store. File reads support
ranges and SHA-256 verification.

VM image runs automatically probe the default socket; when the service is
available they mount the remote image as a read-only FUSE lower, fetch 1 MiB
blocks on demand, and cache them persistently. A missing or stale default socket
falls back to local OCI preparation. An explicitly configured server must work;
`PVISOR_CACHE_SERVER=off` forces local preparation. Explicit rootfs directories
and the native container executor keep their current behavior. See the
[shared image cache protocol](shared-image-cache.md) for the full protocol,
limits and SSH remote access.


Experimental external memory-pool entry points are `pvisor service memory-pool SOCKET` and `pvisor run --vm-memory-pool SOCKET`. Keep the pool running: stopping it fails dependent VMs. See [first-version memory sharing integration](../design/memory-optimization/proof-of-concept.md#v1-integration) for configuration, budgets and usage.
