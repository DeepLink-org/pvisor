# `pvisor` command reference

The Job is pVisor's primary user-facing object: one managed command, its
execution evidence, and any staged filesystem changes. `pvisor run` creates a
Job. The other flat commands act directly on that Job; there is no `job`
subcommand. `replay` starts a Job from a trajectory. Existing Job IDs and on-disk records retain their `run-*`
and `Run Bundle` names.
Full command examples for Host, OCI VM, and transparent host-rootfs VM
are in
[Run workloads with pVisor](../guides/execution.md).

## Find the command you need

Use the smallest surface that matches your next decision:

- **Run a command:** start with [`pvisor run`](../start/first-run.md), then use
  `status --review`, `inspect`, and `apply` to decide what reaches the project.
- **Understand a boundary:** use `status` and `inspect`, then read the
  [execution guide](../guides/execution.md) before changing providers.
- **Continue a trajectory:** use `replay` only when you already have a
  supported trajectory and want a fresh sandbox; begin with the
  [replay guide](../guides/sandbox-replay.md).

If this is your first command, do not start with the full option list below:

```bash
pvisor run --stage ./runs/task-001 -- codex
pvisor status --review last
pvisor apply last --path src
```

Add `--tui` for a Zellij-style terminal frame, bottom status bar, and floating
review panel:

```bash
pvisor run --tui -- bash
```

The shell or Agent keeps the terminal width and receives keyboard input by
default. The header shows the workspace and file/network boundaries. The bottom
bar shows state, elapsed time, pending authorization and nonzero activity or
errors; detailed counters remain in the panels. `Ctrl-]` opens the menu. Pressing the prefix replaces
the same bar with the available shortcuts. pVisor's own startup
diagnostics appear in the Log panel instead of
the Agent terminal. Press `Ctrl-]` to enter command mode, then `r`, `f`, `n`,
`u`, `l`, or `p` to open the Overview, Files, Network, Job, Log, or Permissions panel. Press `?`
for key help. In a panel, use Tab or `1`–`6` to switch views, `j`/`k` to scroll,
and Esc or `Ctrl-]` to return to the Agent. Press `Ctrl-]` twice to send a
literal Ctrl-] to the Agent.

Use `pvisor --ask -- bash` to ask before accessing files covered by `ask`
rules or unlisted proxy destinations; `--ask` also enables `--tui` and `--safe`.
File approval covers inspection, reading, modification, and deletion in the
allowed view, not read-only access. Saved decisions are consulted only for
matching ask rules, and cannot override explicit deny or sandbox boundaries.
Ask prompts build reusable permissions as you work. `--access 'private/*:ask'`
automatically opens the audit TUI. In a prompt, choose where to remember the
answer with `s` (session, the default), `w` (workspace), or `u` (user), then
press `1` to select the exact target and Enter to confirm, or `d` to deny it.
Use Tab or Up/Down to move between scope, lifetime and action buttons;
Left/Right changes the selection. Enter on an action button confirms it.
Deny is selected by default; `d` denies immediately. Selection alone never grants access. For files, `2` allows
files in the same directory and `3` allows the same suffix. For network
requests, `2` allows the hostname and its subdomains on the same port and
transport.

Session rules live in the Job's `audit-policy.json`. Workspace and user rules
live under `permissions` in `~/.config/pvisor/config.toml` (or
`$XDG_CONFIG_HOME/pvisor/config.toml` when set to an absolute directory), and
load when a new TUI Job starts. Workspace rules use the canonical workspace
path. Precedence is session > workspace > user, with the last matching rule
winning within each scope. Explicit deny policies still take precedence over
ask approvals. Persistent file rules use original absolute paths; user-wide
suffix grants can match files across workspaces. Review rules in Permissions;
use `j`/`k` to select a saved decision and `x`, then `x` again, to forget it.
Removing a decision restores matching broader rules or a new prompt; it does
not close existing file handles. Other running TUIs reload on their next launch.
Other configuration settings and comments are preserved when saving.

The reference that follows is organized by lifecycle. Options that affect the
same Job are intentionally described together so that a copied command has a
clear verification step.

```text
pvisor
├── run                 start a Job
├── apply               commit a stopped Job's staged changes
├── drop                discard a stopped Job's staged changes
├── status              show Job state and review its evidence
├── kill                request termination of a live Job
├── fork                start a child Job from a stopped Job
├── inspect             open a read-only Job filesystem view
└── replay              start a Job from an Agent trajectory
```

## Safe first run

```bash
pvisor run --safe --stage ../stage-001 -- codex
pvisor status --review last
```

Host execution preserves the host filesystem view by default. `--filesystem sandbox`
opts into pVisor's synthetic-root/Landlock or Seatbelt filesystem access policy.
`--safe` stages workspace writes and keeps writable home
state in a private copy-on-write view, including Codex launched from a shell.
`--safe` and `--ask` retain workspace changes and `run-bundle.json` in Job
storage by default. Use `status --review`, `apply`, or `drop` after exit.
`--stage <PATH>` chooses another storage directory; it is not required to retain changes.

`--strict` fails closed before command start unless every requested capability
dimension has non-bypassable enforcement evidence. Today host, container, and
VM executors all request Network and Subprocess enforcement, and none claim
Subprocess — so `--strict` currently exits with `UnsupportedPolicy` on those
paths. Use it to verify fail-closed behavior, not as a “stronger sandbox is
ready” switch.
On Linux, `--filesystem sandbox` uses pVisor's rootless launcher with
user/mount/PID namespaces, a minimal bind-projected root, `chroot`, and a
kernel-negotiated Landlock policy. `--overlaynet-deny-all` independently adds a
private network namespace; the
public/allowlist proxy modes remain cooperative. On macOS the host executor
installs a generated Seatbelt policy only when filesystem sandboxing or network
isolation is requested; filesystem policy remains independent from network
policy. Staged writes are non-bypassable. For deny-all Runs it blocks IP and
ambient host Unix sockets,
while retaining the exact AgentCtl and Run-local IPC. Reads and selective
network policy remain ambient/cooperative and are labeled separately in the
Bundle. Native OCI and libkrun executors retain the same outer Run, OverlayFS,
and AgentCtl state observation.

After completion:

```bash
pvisor status --review last
pvisor fork last -- codex
pvisor apply last --all # or: pvisor drop last
```

`fork` snapshots the stopped Job's staged filesystem before starting the child.
Pass `--checkpoint ID` to reuse an existing logical checkpoint. Embedded hosts can call
`RunHandle::checkpoint`: pVisor publishes an AgentCtl quiesce directive,
requires every Session frozen into the checkpoint to report the matching
quiesced state, snapshots the raw upper, then publishes `continue`. Logical
checkpoints preserve filesystem and cooperative client safe-point boundaries,
not process memory.

To stop a running Job, use `pvisor kill JOB_ID`. It requests graceful
termination from the Job supervisor; check `pvisor status JOB_ID` for the final
state. A stopped Job can still be reviewed and applied or dropped.

## `--safe` parameter preset

```bash
pvisor run --safe -- claude
pvisor run --safe --vm --rootfs image=my-agent-image:latest -- codex
pvisor run --safe -- zcode
pvisor run --safe --overlaynet-allow inference.example.com:443 -- zcode
```

`--safe` generates a command-line argument patch, parses it through the same CLI parser, and
applies it before explicit user arguments. The preset matches executable names to restore each Agent's default HTTPS API grants. It requires isolation from the
selected executor and never selects an executor.
Precedence is **explicit CLI > safe preset > configuration
file > ordinary defaults**. Runs without `--safe` retain their existing behavior. The preset
supports commands and TOML specs; prepared JSON RunSpecs reject it.

`--safe` directly requires filesystem read/write and network isolation, without falling
back to a plain host process. There is no separate sandbox flag or configuration setting.
`--strict` continues to validate all requested capability dimensions, including resource
limits, separately.

- macOS host: Seatbelt confines reads/writes and allows only the allocated loopback TCP proxy
  port, with necessary Run-local Unix IPC. Direct IP traffic and ambient host Unix sockets
  are blocked. The Agent gets a temporary HOME; provide credentials explicitly or through
  Gateway. System runtime files and path metadata needed for loading remain readable.
- Linux host: `--safe` requires rootless namespaces, a synthetic root, chroot,
  Landlock, and copy-on-write home state. Selective egress and Gateway traffic use the supervisor loopback proxy
  cooperatively; direct sockets may still bypass it. Select VM or deny-all when a non-bypassable
  network boundary is required. The launcher projects the caller's HOME through a private stage;
  overlay deny globs do not hide secrets at their original paths outside the overlay view.
- VM: the existing `auto` network boundary is required. Safe never selects VM automatically.
- Container: `--safe` is rejected until a complete enforcement boundary is available.

Sandbox setup failure stops execution. `--safe` cannot be combined with `--overlaynet off`.

Only the following HTTPS destinations (port 443) are granted by default.

| Command | Default destinations |
|---|---|
| `codex`, `bash`, `sh`, `zsh`, `fish` | `api.openai.com`, `chatgpt.com`, `ab.chatgpt.com` |
| `claude` | `api.anthropic.com` |
| `gemini` | `generativelanguage.googleapis.com` |
| `zcode` | `api.z.ai`, `open.bigmodel.cn` |

Unknown commands deny ordinary egress. Override the preset destinations explicitly with
`--overlaynet-allow HOST:PORT`. Existing denies and rate limits remain in force;
Gateway capture uses explicitly configured routes.

Without `--safe`, Codex state and project writes reach their host lower paths.
With `--safe`, the workspace uses the reviewable Run stage and HOME (including
`CODEX_HOME` when set) uses a separate private stage. Home-state changes are
discarded when the Run ends and are not part of the workspace Run Bundle.

Independently of `--safe`, direct `zcode` on the Linux rootless host executor
receives a compatibility policy. It inherits the host environment and grants
direct persistent writes to existing `~/.zcode`, `$XDG_CONFIG_HOME` (or
`~/.config`), and `~/.local/share/applications`. If a bundled Chromium setuid
helper is found, pVisor hides it and adds `--no-sandbox`; it also adds
`--disable-gpu`. Chromium's own sandbox is therefore disabled on that path,
while pVisor's outer rootless boundary remains. These state writes bypass the
workspace stage, even under `--safe`. The policy is selected by the direct
executable name, so shell wrappers do not receive it. Gateway profile
`zcode-bigmodel` is a separate routing adapter.

The preset uses `--clear-pass-env` to clear configured `run.pass_env`.
`--clear-pass-env` also works on its own; explicit `--pass-env NAME` grants
are applied afterward. Direct Codex commands inherit the host environment to preserve account and routing
configuration. Other commands default to disabled inheritance; pass credentials with
`--pass-env NAME` (see the independent ZCode compatibility policy above).
Explicit CLI options can restore or override these settings. `--safe` stages the
workspace by default. Existing container mounts and filesystem layers are retained; the project base,
rootfs and executor remain unchanged. Use `--pass-env` to deliver credentials explicitly, or let a configured
Gateway hold the upstream key on the trusted side.

Startup messages report the effective policy and overrides. Without `--safe`, host/container
selective proxies remain bypassable. Hostname rules cannot
distinguish inference from telemetry/upload APIs on the same host or detect data inside model
requests. Glob rules cover the overlay view. On Linux, the projected HOME can
expose sensitive files at their original paths despite those overlay rules;
explicit shares need their own protection. Renamed copies, embedded secrets
and Git history are not identified by filename rules.
Bulk-read and tool-call attribution monitoring are not provided. Additional shares and
`--rootfs host` expand the exposed data. `--safe` cannot be combined with disabling OverlayNet.

## File access rules

```bash
pvisor run --safe --mount /opt/tool:read --access '**/.ssh:deny' --access '**/.env*:warn' -- my-agent
```

`--mount SOURCE:read` grants a host executor read-only access to that absolute
path, and requires `--safe` or `--ask`. `SOURCE:write` grants direct persistent
host writes. Both keep the original path, cannot be remapped, and must not
overlap the workspace, Job storage, or writable runtime paths. Read-only
shares under the private `/tmp` are unsupported on Linux. These explicit
shares are outside overlay-relative ask rules.
`--mount SOURCE[:TARGET]:stage` adds a copy-on-write lower layer to the workspace
view; it is not an independent directory mount. A Run accepts only one
nonidentity TARGET for this view. Use stage layers for VM composition;
read/write host grants are not supported by the VM or container executor.
`--access PATH-GLOB:deny|ask|warn` adds overlay rules: deny blocks access, ask
pauses matching access for approval, and warn allows access with a diagnostic.
The former `:read` warning spelling is rejected; use `:warn` or a read-only share.
Rules accumulate across config, presets, and CLI. `--clear-access` explicitly
removes config and preset file rules before applying CLI rules. It removes the
default sensitive-file protection too. Precedence is deny > ask > warn.

Globs are relative to the mount root: `*` stays within one component, `**`
crosses directories, and a matching directory covers all descendants. Matching
is case-insensitive. Empty patterns, absolute paths and `.`/`..` components are
rejected. Warnings print the path, never file contents, to supervisor stderr;
they are access attempts, not exact read counts.

Safe denies `.ssh` and `.gnupg` directories at any depth and files named `id_rsa`, `id_dsa`,
`id_ecdsa`, `id_ecdsa_sk`, `id_ed25519`, or `id_ed25519_sk`. It warns on `.env`, `.env.*`,
`*.pem`, `*.key`, `*.pub`, `*.p12`, `*.pfx`, `.aws/credentials`, `.netrc`, and `.npmrc`.
Public keys inside `.ssh` are hidden with the directory; those outside only warn. These names
cannot identify every private key; add rules for custom names.

```toml
[filesystem]
stage = "../stage-001"
mount = [{ source = "/opt/tool", access = "stage" }]
access = [
  { path = "**/.ssh", level = "deny" },
  { path = "**/.env", level = "warn" },
]
```

Explicit CLI access entries are appended after configured and safe-preset
entries. Deny takes precedence over warnings. Policies are recorded with the
Run and its overlay artifacts.

Host FUSE and VM virtio-fs share the checks. With any deny rule, this initial implementation
conservatively rejects all multiply-linked regular files and new hard links to prevent alias
bypasses. Directory moves, exchanges and removals check affected subtrees and reject operations
that include denied files. Symlinks resolve through the mount namespace; file opens do not follow
a final symlink into raw backing storage. VM root views also receive rules, with workspace
original paths and overlay backing directories protected. Executor isolation is still required:
ambient host reads, extra container shares and credentials outside the view remain separate
concerns. Backing directories must be managed by a trusted supervisor; concurrent mutation by
other host processes is outside this rule mechanism's guarantee.

## Replay an Agent trajectory

`pvisor replay` assumes the caller has normally created a fresh sandbox. It
replays complete tool batches through `after_step`, rebuilds the selected
Agent native context with fresh observations, and then starts the live Agent:

```bash
pvisor replay \
  --agent claude-code \
  --trajectory /input/session.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/claude \
  --boundary-user-prompt 'Review the fresh observation before continuing.'
```

OpenHands, mini-swe-agent, Pi agent, OpenCode, Codex, and SWE-agent use the model endpoint and
credentials already present in their environment. Pi requires its exact
`0.83.0` runtime and accepts native RPC event JSONL containing the core
`read`, `bash`, `edit`, and `write` tools. Claude Code uses a temporary bridge owned
by SandboxReplay because its native resume transport inserts wake-up messages.
The bridge validates and removes that exact Resume Transport envelope before
forwarding the model request. It does not enable pVisor Gateway, capture model
traffic, or persist a bridge audit.

OpenCode requires the exact `1.17.7` runtime and its native
`opencode run --format=json` event JSONL. Codex requires the exact `0.149.0`
runtime and Codex rollout `response_item` JSONL. Both rebuild the native prefix
in the fresh sandbox and invoke their native resume command for continuation.
Codex derives its native session ID from the trajectory's `session_meta`; the
request `session_id` is only a model-router/Run key and cannot override it.
Continuation fails closed when the native session is missing.

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
length, and a digest; Agent-native prepared or continued trajectories may
contain the user message.

The result schema is `sandbox-playback.result/v3`, with typed `phase`, `quality`,
and `agent_status` fields plus state/output locations, artifacts, and an optional
structured failure. Existing non-Claude callers that used `replay_only = true`
only to construct a prefix must migrate to `prepare_only = true`.

`disable_thinking` belongs to `[replay]` and is also exposed as
`--disable-thinking`. Claude Code's protocol bridge applies it to the upstream
request; OpenCode omits its `--thinking` flag when it is set. It does not turn
on Gateway capture. Optional `[run]`, `[overlayfs]`, and `[overlaynet]` sections
create an outer managed `pvisor run`; they do not change the inner replay model
path.

By default, replay's internal state, WAL, manifest, fresh-observation
comparisons, and native working files remain under
`/tmp/pvisor-sandbox-replay` and disappear with the sandbox. Replay does not
enable pVisor Gateway, a model-traffic capture store, or a Claude Resume
Transport audit. A caller that explicitly selects `--state-dir` or
`--output-dir` owns those files. Use `--replay-only` to execute the prefix and
stop before live inference, or `--prepare-only` to construct it without execution.

## One configuration model

`pvisor run` has one canonical `RunConfig`. The CLI covers its common fields,
but `run.inherit_env` currently has no direct CLI switch. Moreover,
`apply_safe_defaults` currently clears environment inheritance for non-Codex
CLI commands even without `--safe`; the ZCode host adapter re-enables it for
direct `zcode`. Treat TOML `inherit_env` as ineffective on those CLI paths.
`--config` reads an explicit TOML `RunConfig`; `--spec` requires a prepared
JSON `RunSpec` for delegated execution and cannot be combined with other Run
overrides. pVisor does not discover a hidden project file.

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

`--record-destination` writes a local `trace::Event` Journal to
`events.trace.jsonl`. Legacy JSONL is not supported. Journal positions define
append order; `caused_by` defines causal links. `observed_at_unix_ms` is
observation metadata, not an ordering source of truth.

The equivalent TOML is:

```toml
# host (default) or sandbox; independent from OverlayNet and OverlayFS staging
filesystem = "host"

[run]
agent = "my-agent"
executor = "host"
command = ["my-agent"]

[filesystem]
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
entries. Every serialized `[filesystem]` field has a CLI form: `stage`, `mount`,
`access`, and `max_size` map to `--stage`, `--mount`, `--access`,
and `--overlayfs-max-size`.
The size limit is checked after execution, so it does not bound peak space used
while the Agent is running.
The command after `--` replaces `run.command`.

`--container-image IMAGE` selects the OCI container executor automatically;
`--executor container` makes the choice explicit. The transport resolves a
matching static `linux-amd64`/`linux-arm64` pVisor, mounts it into the image,
overrides the entrypoint, and invokes the normal
`pvisor run --executor host --spec ...` path. The workload command is carried
inside the RunSpec rather than exposed in OCI runner argv. The injected
pVisor creates its own AgentCtl and returns a typed RunResult. The final
OverlayFS cwd and session Gateway configuration are mounted at stable paths.
User mounts are repeatable TOML inline tables, for example:

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
`container.network = "host"`, because their injected addresses are host
loopback endpoints. Bridge and no-network modes are valid when these drivers
are off. The executor records container isolation but does not claim full
capability enforcement.

`--executor vm` uses statically linked libkrun and its embedded init to boot a
minimal Linux guest. `--rootfs image=IMAGE` selects this executor and pulls an
OCI/Docker image directly, without invoking Docker, Podman, or Buildah. When no
explicit rootfs or image is supplied, VM execution uses the host `/` through
virtiofs and OverlayFS on Linux, preserving the host runtime, PATH, and HOME.
On macOS, supply a Linux rootfs or image explicitly. Manifests
and layer digests are verified, the host architecture selects `linux/arm64` or
`linux/amd64`, and the unpacked rootfs becomes the immutable lower layer of a
pVisor OverlayFS. `--image-store` overrides the platform cache directory.
OCI cache targets are marked immutable, and this protection survives logical
checkpoint/fork, so `pvisor apply` cannot mutate a rootfs shared by other Runs.

On Linux, `--rootfs host` selects the host `/` as the VM rootfs lower and
selects the VM executor when `--executor` is omitted. `--rootfs <PATH>` selects
a prepared directory and `--rootfs image=<PATH>` selects an OCI image or image
path. These forms are mutually exclusive, and host rootfs is rejected on macOS.
Use `--mount SOURCE[:TARGET]:stage` for additional lower layers in the guest
workspace view; the current workspace is the implicit bottom layer. Workspace
changes are retained in the configured stage or default Job storage. Writes elsewhere in the VM root
use a temporary upper and are discarded when the VM exits.

The merged rootfs is guest `/`, and `/workspace` becomes the guest cwd. On both
Linux and macOS, a vendored libkrun serves pVisor's rootfs and workspace
copy-on-write unions directly over virtio-fs. The VMM never re-exports a host
FUSE mount and does not materialize or reconcile either tree. Linux uses
KVM and Apple Silicon macOS uses HVF through the same executor. Linux static
musl builds embed the guest kernel and require no firmware shared library at
runtime; `--vm-library-dir` is rejected by these builds. macOS wheels install
`libkrunfw.5.dylib` beside pVisor. macOS source runs otherwise download the pinned
official release into a SHA-256-verified platform cache, where `/usr/bin/cc`
turns its prebuilt kernel bundle into the required dylib. On macOS,
`--vm-library-dir` selects an existing firmware directory. OverlayNet `auto` uses the
non-bypassable VM smoltcp IPv4 TCP/DNS driver, while Gateway capture uses an
internal route through the guest virtual router. Linux additionally confines
the VMM with namespaces and Landlock. The macOS VMM still has the invoking
user's host permissions, so the first OCI-image version must not be treated as
a hostile multi-tenant boundary despite the guest-kernel isolation.

On host/container execution, the four visible OverlayNet policy flags and
Gateway capture automatically enable the proxy driver. `--safe` stages the
workspace by default; `--mount` adds explicit layers. An explicit `--stage`
retains the filesystem state for review. When a stage is nested inside a base or compose layer, pVisor hides
that subtree from the merged view and rejects guest attempts to recreate it.
libkrun Runs create no live host mountpoint, preventing host indexers from
recursively entering `<stage>/merged`. The reverse topology, where a
stage contains a lower layer, is rejected. Applying a composed Run is rejected
until pVisor can materialize a complete merged-vs-base diff safely.
On host/container execution, OverlayNet policy applies to traffic routed
through the explicit proxy and does not claim non-bypassable host network
isolation. On a libkrun VM, `auto` attaches non-bypassable smoltcp IPv4
TCP/DNS; `off` leaves the guest offline. `--overlaynet-deny-all` supplies the
same default-deny policy to the active driver. Host/container direct sockets
remain ambient, while a VM Gateway route remains available through the guest's
virtual router for configured model traffic.

## Run project discovery

The current directory is the default project association. `--mount` identifies
additional host layers and, when specified, their Agent-visible paths. Each Run receives an
independent directory under pVisor's default records root. If that root would be inside
the selected OverlayFS base or a compose layer, pVisor instead uses the system
temporary Run root to keep the writable stage disjoint:

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
Terminal cleanup removes `upper`, `work`, and other disposable staging data but
retains compact Run/Overlay metadata, the apply ledger, and capture artifacts.

## Related workflows

- [Your first Job](../start/first-run.md) for the shortest complete loop.
- [Execution environments](../guides/execution.md) for choosing a provider.
- [Review and apply changes](../guides/review-apply.md) for filtered, repeatable apply.
- [Network control](../guides/network.md) and [capture](../guides/capture.md) for other Effect dimensions.

### Shared image file cache

`pvisor cache serve` runs the OCI file service in the foreground. Use
`cache prepare IMAGE`, `cache list DIGEST [PATH]`, `cache stat DIGEST PATH`, and
`cache read DIGEST PATH` to query it. `PVISOR_CACHE_SERVER` selects the
endpoint; the default is `pvisor/cache.sock` under the user's cache
directory. The server accepts `--image-store DIR` for an existing OCI store.
Reads support byte ranges and SHA-256 transfer verification.

VM image runs automatically probe the default socket and use a read-only FUSE
lower with persistent 1 MiB block caching when a compatible server is available.
A missing/stale default socket retains local OCI preparation. An explicitly
configured server must work; `PVISOR_CACHE_SERVER=off` forces local
preparation. Explicit rootfs directories and native containers are unchanged.
See the [protocol and remote-access guide](https://github.com/DeepLink-org/pvisor/blob/main/docs/shared-image-cache.md).
