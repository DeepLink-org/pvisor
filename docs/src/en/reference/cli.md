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
chooses the storage location and is no longer a prerequisite for retaining
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
pvisor
├── run                 Create a Job
├── apply               Apply staged changes of a stopped Job
├── drop                Discard staged changes of a stopped Job
├── status              Inspect Job state and review evidence
├── review              Review current or saved workspace changes
├── checkpoint
│   ├── create          Save the workspace of a stopped Job
│   ├── list            List Job checkpoints
│   ├── show            Inspect ownership and retained references
│   ├── delete          Delete unreferenced checkpoints
│   └── gc              Collect leftover workspace transactions for this Job
├── kill                Request termination of a running Job
├── fork                Create a child from a stopped Job
├── inspect             Inspect the Job filesystem read-only
└── replay              Create a Job from an agent trajectory
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
in the Bundle. Native OCI and libkrun executors retain the same outer Run,
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

Current GC only removes checkpoint staging and deletion directories for the
selected Job, reporting scope `job_workspace_transactions`. Shared execution
content-store GC is not yet connected. Reading or forking historical checkpoints
still requires the source Job lease, so these operations refuse a running source.

`suspend JOB`, `resume JOB`, `checkpoint create JOB --kind execution` and
`fork JOB --state execution` currently return `CAPABILITY_UNSUPPORTED` without
changing Job state. Overlay/DAX, temporary root filesystem layers and Attempt
handoff for ordinary VM Jobs are not yet connected to full save/restore. The
existing standalone `snapshot` workflow remains available; its objects are not
Job checkpoints within a stage. See [Job checkpoint design](../design/job-checkpoint-cli.md#10-当前实现与验收边界)
for the implementation scope.

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
generates a standard OCI bundle, resolves a matching static
`linux-amd64`/`linux-arm64` pVisor, mounts it into the rootfs, sets the process
args, and takes the normal `pvisor run --executor host --spec ...` path. The
Agent command is carried inside the RunSpec rather than exposed in OCI runner
argv. The injected pVisor creates its own AgentCtl and returns a typed
RunResult. The final OverlayFS cwd and session Gateway configuration are mounted
at stable paths.
`--container-rootfs PATH` supplies an existing rootfs directly; otherwise pVisor
prepares `--container-image` from its bundled OCI image store. The runtime must
be `runc` or `crun`; Docker/Podman are no longer invoked. User mounts are
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

The in-process Gateway and explicit OverlayNet proxy currently require
`container.network = "host"`, because their injected addresses are host loopback
endpoints. `none` mode is valid when these drivers are off; `bridge` requires
external CNI configuration and is currently rejected. The executor records
container isolation but does not claim complete capability enforcement.

`--executor vm` uses statically linked libkrun and its embedded init to boot a
minimal Linux guest. `--vm-ram-backing FILE` (`[vm].ram_backing`) creates a new
private file backing live RAM. When omitted, an attempt-local file in the user
cache is deleted on normal exit. `--vm-ram-compression`
(`[vm].ram_compression = true`) selects the PVZRAM v2 manifest and immutable
Zstd Seekable base/delta sidecar files
at startup, requiring Linux FUSE or the macFUSE kernel backend.
Rust `RunHandle::pause/resume/offload` controls
the VM; a new offload destination must be on the backing's existing filesystem.
Reclaim reports sampled residency, not guaranteed zero RAM. The file is not a
complete VM snapshot. This implementation has not been compiled or validated at
runtime.

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
Linux and macOS, a vendored libkrun serves pVisor's rootfs and workspace
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
and rejects guest attempts to recreate it. libkrun Runs create no live host
mountpoint, preventing host indexers from recursively entering `<stage>/merged`.
The reverse topology, where a stage contains a lower layer, is rejected. Until
pVisor can safely materialize a complete merged-vs-base diff, a composed Run
rejects a subsequent `pvisor apply`. Selective host/container network rules
apply to traffic through the explicit proxy. Host deny-all uses a namespace or
Seatbelt to block direct egress; containers can use `--container-network none`
for offline execution. On a libkrun VM, `auto` uses smoltcp IPv4 TCP/DNS and
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

`pvisor cache serve` runs the OCI file service in the foreground;
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


Experimental macOS memory-pool entry points are `pvisor memory-pool SOCKET` and `pvisor run --vm-memory-pool SOCKET`. Keep the pool running: stopping it fails dependent VMs. See [first-version memory sharing integration](../design/memory-sharing/index.md#v1-integration) for configuration, budgets and usage.
