# pVisor Agent plugins

Run a bounded task from Pi, DeepSeek Harness (DSH), or Codex, inspect its staged
files, and accept selected results. These are **task adapters**, not replacements
for the host's built-in filesystem, terminal, or background-job tools. Each
`pvisor_spawn` starts a new native Session; later calls do not continue that Session.
Put dependent edits and tests in one `argv`, or explicitly fork a stopped
retained file candidate into a new Session for the next attempt.

Use plugin version 0.1.1 or newer for a separately installed runtime. The
published 0.1.0 package requires obsolete npm runtime packages; reinstalling it
will not switch to Homebrew/pip. Remote marketplace refs require separate
publication; npm publication does not publish a marketplace tag.

The output, Rust profile, fanout and batch-cleanup additions started in 0.1.2.
Version 0.1.3 removed additional drop confirmations; 0.1.4 added detailed previews.
Version 0.1.5 made approval cards compact and required Agent intent in DSH.
Version 0.1.6 added native Host workspace fork, scoped artifact export and
structured review. Version 0.1.7 added a read-only DSH task monitor; 0.1.8 fixed
its package-name discovery. Version 0.1.9 improved task browsing and ordering;
Version 0.1.10 removed persistent explanations; 0.1.11 fixed refresh churn.
The current 0.1.12 test build presents task runs with human-readable titles,
one primary status and per-file inline diffs.
Full diff stays in preview results; apply still requires approval.
Registry commands require publication; source tests do not publish it.
See [the changelog](CHANGELOG.md) for caller changes and acceptance scope.

Node.js 22.19+ and a separately installed pVisor runtime are required. Install
pVisor through Homebrew or pip before installing the plugin. Host staging still
needs the system's FUSE/FSKit and namespace support on Linux x64 or macOS arm64;
the plugin cannot install privileged OS components.

## Install

Install the runtime using one of these methods:

```sh
brew tap DeepLink-org/formula https://github.com/DeepLink-org/Formula.git
brew install DeepLink-org/formula/pvisor
```

Or use a Python 3.10+ virtual environment:

```sh
python3 -m venv ~/.venvs/pvisor
source ~/.venvs/pvisor/bin/activate
python -m pip install pvisor-sandbox
pvisor --version
```

Follow the [installation guide](https://deeplink-org.github.io/pvisor/start/installation/)
for platform prerequisites. Launch your Agent host with `pvisor` on its PATH.
Desktop apps may not inherit your terminal PATH; set `PVISOR_PLUGIN_BINARY` in
the host environment to the absolute installed CLI path if needed. The plugin
never downloads a runtime or installs packages. It checks required CLI flags
before work and refuses incompatible runtimes; older releases such as 0.3.1
without selective apply are insufficient. Plugin and runtime versions are independent.

Activate only the Agent hosts you use.

### Pi

```sh
pi install npm:pvisor-agent-plugins@0.1.12
```

Start a new Pi session in the target project. Ask it to call `pvisor_doctor`,
then `pvisor_spawn`. The extension adds tools without overriding Pi's native tools.
For whole-Agent staging, launch Pi itself through `pvisor spawn --safe` instead;
do not nest task plugins inside that outer Session.

### DSH Web and Desktop

```sh
dsh plugin --profile web add pvisor-agent-plugins@0.1.12
dsh plugin --profile desktop add pvisor-agent-plugins@0.1.12
```

Install only the profile you use, then restart it. Desktop also accepts
`pvisor-agent-plugins@0.1.12` in **Plugins → Add plugin**. The package
uses Cordis's functional plugin interface and `dsh-tools.defineTool`; its peers
target DSH 0.2.x. It does not disable existing providers or change permission
presets. Check that `pvisor_doctor` appears in the actual tool catalog.

In DSH Web, expand the right Sidebar and choose **pVisor** from its guide
(or the tab strip's add control). The read-only panel lists the current
Session project's tasks, separate execution/stage states, selected candidate
entries and captured terminal output. It uses the same Backend as the tools,
not direct access to native state files. The browser extension targets DSH
**0.2.2-rc.1**; older alpha versions' tool compiler tests do not establish UI
compatibility. Check `dsh --version` before installing this test build.
Desktop/Electron UI acceptance is not claimed.

Use 0.1.8 or newer for the monitor: 0.1.7's bundle mounted a named `/dsh`
subpath, which DSH 0.2.2-rc.1 skips during browser-module discovery. Backend
tools can work even when the panel is absent. The corrected bundle mounts
the package root, which exports the same Host plugin; `/dsh` remains an alias.
After upgrading, stop/restart DSH Web and refresh the browser.

The visible panel samples sequentially one second after a read when a page
contains live/running/unknown tasks, otherwise after two seconds. Hidden tabs,
background Sessions and hidden browser pages stop sampling and abort reads.
This is background sampling, not server push or a live output stream; very
short tasks can finish between observations. Background reads never toggle
the loading indicator or clear existing rows. Timestamp-only replies retain
the existing objects and publish no UI update. Details are recollected on row
changes/manual refresh, with continued observation when a selected task moves
off-page. Pages contain up to ten tasks and provide
previous/next navigation; they are not a project-wide count or an atomic
snapshot. Errors retain the last observation with an explicit stale warning.
Changes distinguish files/directories and show at most 200 entries with an
explicit limit notice; use `pvisor_review` for the complete bounded inventory.
Output is on demand, terminal-only and bounded, not a live log stream.
There are no run/apply/drop/fork/export controls, automatic cleanup or acceptance.

The task list uses DSH's shared Button/Pill/Tag controls and theme tokens.
Rows show the retained fanout name or a command-derived title, a secondary
monospace command excerpt without added argv quotes, one primary status,
relative time and elapsed duration. These display heuristics are never commands
to relaunch. Detailed dates use Chinese 24-hour formatting with the browser's
time zone. It sorts by creation time **before pagination**. New tasks
record adapter creation time; legacy task-record birthtime (or mtime fallback)
is explicitly estimated with `≈`, not represented as native execution time.
`pvisor_tasks` retains legacy ID ordering by default; `order: "newest"` selects
this creation-time order. Keep the same order when using `next_cursor`.

**待处理** hides confirmed discarded/dropped/applied rows on the current page;
**全部** shows them. Filtering is page-local, not a scan of all tasks;
older retained tasks may be on another page. The project name opens collapsed
project/sorting/history information; only a short read-only badge is persistent.
Empty pages offer **查看全部**, with pagination shown only when available.
Discarded stages are
**已清理**: native drop clears candidate upper/work data, while task
metadata and historical execution evidence remain. This is not disk-usage
measurement or automatic garbage collection. Click a task for **文件变更**
or **终态输出**. File rows show Chinese change labels and `+N −M` from validated
native diff hunks. Selecting a file displays its inline diff alongside the list
(stacked in narrow panes), not behind a separate task-level diff tab.
Directories have no invented size/count; binary, unavailable and truncated
diffs keep unknown (`null`) line statistics, not fabricated zeros. Files with
control/non-UTF-8 names retain their inventory but text framing is not guessed.
The view caps file rows at 200 and rendered diff lines at 2000 with notices.
Discarded file candidates cannot
be recovered from the now-empty stage. Diff and inventory are separate reads.

With a source-built runtime that provides `exec --session` and
`status --lifecycle`, a running Linux Host task exposes an **打开终端** action.
It opens DSH's own terminal and replaces its bootstrap Bash with
`pvisor exec --session -it STAGE /bin/sh`. The command starts inside the
existing sandbox, with its original cwd, environment and isolation policy.
Closing this terminal ends only its exec processes; ending the main task
ends all attached shells. Paused/stopped tasks are refused, never resumed or
relaunched implicitly. Landlock-only, VM and non-Linux Sessions are unsupported.
This requires a newly started task and the new runtime, not merely reinstalling
the plugin against the existing released 0.3.2 binary. DSH must expose its
public `webTerminals` and `sidebarRight` services and offer Bash.

Task polling uses the optional native `status --lifecycle --json` projection,
which does not enumerate stage files, checkpoints or apply ledgers. Missing
counts remain unknown. Older runtimes retain their full-status fallback.
Offline completion still validates its Bundle; this is not constant-cost
history lookup. Review currently remains a bounded whole-inventory operation,
not native pagination or single-file diff retrieval.
An interactive continuation needs a new isolated workspace branch and a
managed PTY, not `cd` into private stage directories or an ordinary Host shell.

### Codex CLI

Register the remote marketplace and bind the tools from your project directory:

```sh
codex plugin marketplace add DeepLink-org/pvisor --ref plugins-v0.1.12
codex plugin add pvisor@pvisor
codex mcp add pvisor -- npx --yes --package pvisor-agent-plugins@0.1.12 pvisor-agent-mcp --project "$PWD"
```

The marketplace installs the workflow skill. The native `codex mcp add` command
separately binds its runtime tools to an explicit project. Start a new session
after both steps. Clients without plugin support can use MCP alone, without the
workflow skill. There is no duplicate bundled MCP registration.

`npx` installs only the pinned plugin, without a global npm install. Native
`pvisor` commands come from your Homebrew or pip installation.

This split is intentional: Codex 0.164.0 starts bundled MCP servers in the plugin
cache, and portable manifest cwd paths are plugin-relative.
Do not infer the project from that cwd or rely on project-variable interpolation.
The server rejects missing project configuration instead of falling back.
Run `pvisor_doctor` and check its project before running any task. MCP does not
intercept Codex's built-in `exec`, patch, filesystem, or other MCP tools.
Whole-Agent staging remains the existing
[Codex launch workflow](../docs/src/en/guides/agents/codex.md).

## Configure

Configuration comes from the host process environment, never tool arguments or a
project-controlled config file:

| Variable | Default | Meaning |
| --- | --- | --- |
| `PVISOR_PLUGIN_BINARY` | `pvisor` on the host's PATH | Absolute installed CLI path; an invalid override never falls back |
| `PVISOR_PLUGIN_STATE_DIR` | `~/.local/state/pvisor/agent-plugins/v1` | Private task records and stages, outside the project |
| `PVISOR_PLUGIN_PROJECT` | Unset | Explicit project for manually launched MCP; prefer native `codex mcp add ... --project DIR`; Pi/DSH use their call context |
| `PVISOR_PLUGIN_EXECUTION` | `auto` | VM-first; `vm` requires VM, `host` explicitly chooses the safe Host sandbox |
| `PVISOR_PLUGIN_ALLOW_HOST_FALLBACK` | `false` | Only literal `true` authorizes `auto` to choose Host after a valid negative VM preflight; never retries a launched task |
| `PVISOR_PLUGIN_VM_ROOTFS` | Unset | Operator-prepared Linux rootfs with guest tools; owned/non-writable-by-others, disjoint from project/state; host `/` is forbidden |
| `PVISOR_PLUGIN_RUST_TOOLCHAIN` | Unset | Operator-selected actual Rust toolchain root with `bin/cargo`, `bin/rustc`, `bin/rustdoc`; read-only, offline, no host Cargo HOME shared |
| `PVISOR_PLUGIN_EXPORT_DIR` | Unset (export disabled) | Existing owned `0700` artifact root, disjoint from project, plugin state and configured toolchain; model output is only one new child name |

Run commands always use `--safe`, `--no-agent-defaults`,
deny-all networking, captured stdio and a runtime deadline (1–600 seconds,
default 120). Execution defaults to VM-first. Personal Agent presets are not loaded. No credentials are
explicitly passed into the task. Each task gets a fresh private HOME and XDG
state paths; host login state, user configuration and HOME caches are not inherited.
Durable workspace stages stay in the configured state directory. Scratch HOME
may remain after a failed/interrupted execution; it is not an accepted project
result. Arbitrary networking/executor overrides and task stdin are not model inputs.
Do not remove safety flags to make a failed task work.

### Choose the execution environment

This working-tree addition requires a native runtime with `pvisor vm-check --json`.
An older runtime is refused in `auto`/`vm` mode; explicitly selecting `host`
retains the old Host workflow without requiring the new preflight command.

```sh
pvisor vm-check --json
env PVISOR_PLUGIN_VM_ROOTFS=/absolute/prepared-linux-rootfs dsh web
```

The rootfs must contain the shell, toolchain and dependencies your tasks need.
It is a trusted operator input, not a downloaded/default host filesystem.
The plugin does not install images, firmware, tools or change device permissions.
Linux checks opening `/dev/kvm`, its API version and `KVM_CREATE_VM`; Apple
Silicon checks creating/destroying an HVF VM. Firmware discovery is separate:
`bundled_unloaded` does not establish that the library loads or the guest boots.
Other platforms, including Windows, do not gain plugin support from this option.
Doctor reports the chosen environment and blockers. `probe: true` runs an actual
staged task only after preflight admits an environment; its exact stage is retained.

To authorize fallback once in the Agent host's launch configuration:

```sh
env PVISOR_PLUGIN_ALLOW_HOST_FALLBACK=true dsh web
```

`auto` then chooses safe Host when a valid preflight reports unavailable
virtualization, missing firmware or missing rootfs. Invalid configuration,
malformed/interrupted preflight and old runtimes remain errors. After a VM task
is launched, any boot failure or uncertain outcome stays with that task:
no retry on Host. `PVISOR_PLUGIN_EXECUTION=vm` never falls back, even with
fallback permission. To explicitly keep the previous environment:

```sh
env PVISOR_PLUGIN_EXECUTION=host dsh web
```

Use the same environment variables when launching Pi or Codex/MCP.
DSH rows/details show VM or Host and label authorized fallback. The stored
selection describes the chosen backend, not proof a failed task reached execution.
Existing tasks retain their backend. Host workspace fork now requires explicit
Host policy or `auto` with Host fallback permission; `vm` policy refuses it.
VM workspace fork and Session terminal attachment are not implemented by these
Host-only tools. Container is not yet a plugin policy option.
The offline Rust share profile remains Host-only; for VM tasks, install a Linux
toolchain inside the rootfs and unset `PVISOR_PLUGIN_RUST_TOOLCHAIN`.

The private task record contains its schema, project, task ID, creation time,
optional display name and selected execution policy/fallback reason. It does
not persist command arguments or credential values.
Runtime-owned records and
captured command output have their normal retention rules. Avoid printing secrets.
This is not an isolation boundary against another process with your host UID.

## Run and review

Ask your Agent to use this tool input:

```json
{
  "argv": [
    "sh",
    "-c",
    "printf 'candidate\\n' > accepted.txt; printf 'temporary\\n' > discarded.txt"
  ],
  "timeout_seconds": 120
}
```

`pvisor_spawn` returns the exact stage selector and separate task and CLI results:
top-level `stdout`/`stderr` contain captured task output, bounded to 256 KiB per
stream; `cli` contains the bounded launcher diagnostics. `exit_code` is the CLI
exit, `task_exit_code`/`task_state` come from native validated review, and
`truncated` preserves native and adapter output truncation. The adapter reads
`status --review --json` with an 8 MiB collection budget, not private completion
records. Missing/invalid/oversized completion returns `output_status: unavailable`
and `output_error`, never fabricated success. Interrupted owners are not inspected
automatically; query the same selector later with `pvisor_output`.
A nonzero exit, cancellation, or lost completion can leave
candidates. Do not automatically rerun an uncertain task. Use that exact selector;
the adapters never use `last`.

| Tool | Effect |
| --- | --- |
| `pvisor_doctor` | Check CLI and environment; optional `probe: true` starts one real safe task and retains its selector. Startup success is not isolation certification |
| `pvisor_spawn` | Call native `pvisor spawn` for one new staged task and retain its Session |
| `pvisor_fork` | Start a new command from retained Host file state; explicit source/argv, inherited native policy and timeout, independent child selector and lineage |
| `pvisor_export` | Call native export for explicit paths to a new child directory under the operator artifact root; no acceptance, source consumption or extra approval |
| `pvisor_output` | Recover bounded captured output from a terminal task, without rerunning it |
| `pvisor_tasks` | Paginate this project's task selectors and native status; `limit` 1–50, default 20 |
| `pvisor_fanout` | Run 1–8 named independent tasks, concurrency 1–4; collect outputs and informational elapsed ordering |
| `pvisor_status` | Query the exact Session's JSON status |
| `pvisor_review` | Current structured changes and scope counts, plus optional bounded text diff |
| `pvisor_apply_preview` | Call native `pvisor apply --dry-run` for explicit relative paths |

File acceptance happens after the native task exits. Runtime ownership,
completion receipts and conflict checks remain authoritative. A successful CLI
exit is not proof of a successful user task or sufficient evidence to accept it.

`pvisor_review({selector, include_diff: false})` returns current native `changes`
with `path`, optional lossless `path_bytes`, `kind`, `old_type`, `new_type`,
`size_bytes` and `mode`. Paths are relative to the project. `counts` separates
`file`, `directory`, `symlink`, `other` and `unknown`; deleted entries use the
old type, all others the new type. `in_scope`/`outside_scope` are native
project/environment entry counts, not file counts. Native classification and
unsupported entries remain available in `handoff`; read identity/generation
is in `review_context`. Partial apply/drop refresh the inventory, not execution
history. Native `pvisor_status` fields are unchanged.

Default `include_diff: true` preserves bounded text in `stdout`, with
`diff_status: "separate_observation"` on a successful untruncated read: the CLI
cannot return JSON and diff together, so they are not one atomic snapshot.
Other diff states are `not_collected`, `truncated` or `unavailable`.
`review_status: "unavailable"` has `changes: null` and `counts: null`, never
a fabricated empty list. Unclassified scope counts are `null`, not zero.
Review does not test candidates, guarantee conflict-free apply, or authorize
unsupported apply/export entries.

### Branch retained work

Call `pvisor_fork` with the exact parent `selector` and an explicit `argv`.
The parent must be a stopped retained Host workspace with validated completion
and reconstructable native policy. Public completion evidence must confirm
enforced filesystem/network boundaries and explicit deny-all networking.
The native CLI creates the logical checkpoint,
restores files/preimages into a new stage and runs the child. No adapter file
copy, policy fallback, VM execution restore or arbitrary checkpoint selection is used.

Fork inherits the source's safety, shares and native deadline; there is no
`timeout_seconds` override. The adapter's transport watchdog is 630 seconds.
The child gets fresh private HOME/Cargo state, collected output and native
`lineage`. Changing operator configuration does not grant new shares to a fork;
start a fresh parent when the toolchain/profile changes. Parent and child have
independent selectors; neither is automatically accepted or discarded. This
forks a file candidate, not process memory, Agent conversation or all host lowers.

### Export artifacts without accepting

Before starting the Agent host, configure a dedicated artifact root outside the
project and runtime state, for example:

```sh
mkdir -m 700 "$HOME/pvisor-artifacts"
export PVISOR_PLUGIN_EXPORT_DIR="$HOME/pvisor-artifacts"
```

Reuse an existing root only after verifying ownership and permissions. The
plugin does not create or chmod arbitrary operator roots. Doctor reports the
configured root and source-dependent capabilities.

Call `pvisor_export` with `selector`, explicit project-relative `paths` and
`output`, a single new directory name such as `case-a`. Absolute/nested output
paths are rejected. It invokes native `export --output ROOT/case-a --path ... --json`.
Linux directory uppers only; native completion, type, destination and lock
checks remain authoritative. Existing destinations are never replaced.

`destination`, `manifest_path` and the native `manifest` identify the artifact.
`export_status: published` means the new artifact directory was published, not
that the task succeeded, code was accepted, or anything was uploaded. Failed
tasks may export `partial_results`. Files go under `files/`; deletions and
directories are inventory entries, not filesystem operations. This is not a
merged workspace, complete patch, dependency closure or preservation of all metadata.

Export does not consume the source. Export evidence before full apply/drop
removes staging data. Cancellation or a lost/invalid response returns
`export_status: unconfirmed` with the exact destination; inspect it before any
new attempt. No automatic retry, overwrite, artifact deletion or source drop
occurs. The export transport watchdog is 120 seconds.

### Rust offline environment

Set `PVISOR_PLUGIN_RUST_TOOLCHAIN` in the Agent host environment to an installed
toolchain root, for example the parent of `bin` returned by `rustup which rustc`.
Do not point it at `~/.cargo` or rustup's shim directory. The plugin grants
only that directory read access, sets direct rustc/rustdoc paths and uses a fresh
private `CARGO_HOME` and `CARGO_TARGET_DIR`, with `CARGO_NET_OFFLINE=true`.
The operator must trust the selected tools. It does not install toolchains or
share Cargo credentials, registries or user config.

Prepare locked dependencies with `cargo vendor` outside the sandbox and merge
its generated source configuration into the project's `.cargo/config.toml`.
Then ask `pvisor_spawn` to run `cargo test --offline --locked`. See the
[complete offline setup](../docs/src/en/guides/agents/plugins.md#rust-offline).
Build scripts still need their own system tools/libraries; a visible cargo
executable alone does not establish that a project builds.

### Named experiments and explicit cleanup

`pvisor_fanout` accepts `tasks` containing `{name, argv, timeout_seconds}` and
`concurrency` (default 2). It runs at most 8 tasks with at most 4 in flight,
collects each exact selector/output and returns `elapsed_order` for completed
tasks. This is not correctness ranking or benchmark evidence. Nothing is
retried, accepted or dropped automatically.

The same driver is available as `pvisor-agent-fanout --project DIR --input FILE`
for an operator-owned JSON experiment plan. Use plugin tools when your host
terminal cannot safely launch pVisor. Plans may not configure shares, credentials,
executors or networking.

Use `pvisor_tasks` to recover selectors instead of copying them from chat.
Select explicit candidates for DSH's `pvisor_drop_batch` or Pi's
`/pvisor drop-batch {"selectors":["/absolute/stage-a","/absolute/stage-b"]}`.
Drop does not ask for additional confirmation. All explicitly listed selectors
(1–50) are validated before any mutation, then native drop rechecks each Session. Partial
failure and cancellation are reported per task; there is no rollback. A terminal,
fully applied Session with no candidates or pending apply returns a successful
`already_applied` no-op, not `dropped`; batch cleanup does not count it as failure.
Unknown, live, suspended or otherwise blocked Sessions retain native refusals. Task records remain, and
failed/uncertain private HOME paths are returned as `scratch_home`. No TTL or
directory deletion silently destroys pending work.

### One Agent per instance

Task tools do not bind an Agent's native filesystem or shell to a Session.
For one continuous staged Agent process, use the existing whole-Agent launch
workflow and do not nest task plugins inside it. For repeated commands in a
long-lived Linux VM, container `run`/`exec` with staged project binds provides a
separate lifecycle; it is not native Session continuation. A DSH per-Agent
binding still needs host tool routing so native shell/file tools cannot silently
operate outside that instance. No such interception is claimed by this plugin.

### Pi decisions

Pi requires interactive confirmation for apply; drop commands run without another prompt:

```text
/pvisor status STAGE_FROM_RESULT
/pvisor review STAGE_FROM_RESULT
/pvisor apply {"selector":"STAGE_FROM_RESULT","paths":["accepted.txt"]}
/pvisor drop STAGE_FROM_RESULT
```

Apply first shows a readable change summary and asks for confirmation; without an interactive UI,
apply is refused. Single and batch drop need no confirmation or interactive UI.
Tool calls do not expose apply, so model-generated JSON cannot authorize publication.

### DSH decisions

DSH additionally exposes `pvisor_apply` (`selector`, `paths`) and `pvisor_drop`
(`selector`). Both require an initiating Agent. Apply requires an explicit
`allowed-once` answer from the host approval service, including its native
preview; rejection, cancellation or unavailable approval cannot publish files.
The compact approval card leads with an unverified Agent explanation, followed
by project, actual change count, at most three paths, remaining counts and a task
identifier. It contains no raw diff or full state-directory selector, and remains
readable when the host collapses whitespace. Full selected changes, dependencies
and bounded text diff stay in the preview result.
Conflicts appear as readable blockers, not a JSON dump; blocked/invalid previews
never ask for approval. Native JSON stays in `stdout`, with `summary`, `diff`,
`preview_state` and `approval_ready` available separately.
`0.3.2` requires separate JSON/text dry runs. The adapter rechecks the native plan
after collecting text and refuses changed plans or failed detail collection.
The summary does not certify tests or provide a transaction across files.
DSH apply requires `summary` (1–512 bytes) explaining what changes and why based
on review, in the user's language. Pi operator commands may omit it. The text is
displayed as unverified Agent intent, never forwarded as native authority or tests.
Single and batch drop do not call the approval service. They still validate the
explicit selection and project ownership, respect cancellation and retain native
lifecycle admission. Batch drop always returns per-task outcomes, including refusals.
Drop discards candidates without another chance to confirm; it never rolls back
applied files. No idle cleanup or implicit selection is introduced, and unrelated
host permission settings are unchanged.

### Codex and terminal decisions

Codex's MCP surface intentionally has no apply/drop tools. After reviewing the
diff, run these commands yourself in the project:

Save the returned selector in your terminal as `stage`, then use the native CLI
installed through Homebrew or pip:

```sh
pvisor status "$stage" --json
pvisor review "$stage" --diff
pvisor apply "$stage" --path accepted.txt --dry-run --diff
pvisor apply "$stage" --path accepted.txt
pvisor drop "$stage"
```

The native CLI is an operator interface, not an extra approval mechanism for
Agent terminal tools. A skill cannot prevent an Agent that already has host
shell access from invoking it. Keep native host approvals. No second operation
CLI or commit/abort vocabulary is introduced.

Check `accepted.txt` in the host project and rerun the relevant tests. Drop only
remaining unwanted candidates; skip it if all candidates were accepted. Apply
does not undo external APIs or previously applied files. No operation auto-applies
or auto-drops on Agent idle, shutdown, or plugin unload.

## Verify installation

In a new disposable project, ask your Agent to call `pvisor_doctor`, then run the
two-file example above through `pvisor_spawn`. Check that the host has neither
file yet, review both, accept `accepted.txt`, and drop the other candidate.
The accepted file must remain. Check rejection as well as approval in Pi or DSH.
Keep `--safe` if startup fails; fix the reported platform prerequisites instead.

Source validation used Pi 0.79.3's actual extension loader and DSH
`dsh-tools` 0.2.1-alpha.2's actual `defineTool` compiler. Codex CLI 0.164.0
accepted the local marketplace package in a disposable `CODEX_HOME`.
Those checks cover package/schema loading, not interactive user decisions.

No cloud-model or interactive acceptance result is claimed by those loader tests.
Developer tests, source installs and publication instructions live in
[CONTRIBUTING.md](https://github.com/DeepLink-org/pvisor/blob/main/plugins/CONTRIBUTING.md).

## Upgrade and uninstall

Review pending tasks before upgrading. Update the plugin through the host's
package manager and the runtime separately through Homebrew or pip, then restart
and run Doctor to check compatibility. Update a Codex pinned marketplace ref and
its `npx` version together. Never replace a runtime underneath a running task.

Use `pi remove npm:pvisor-agent-plugins`,
`dsh plugin --profile web remove pvisor-agent-plugins` (or the selected Desktop
profile), and `codex plugin remove pvisor@pvisor` as supported by each
host's help. Remove its separate MCP registration with `codex mcp remove pvisor`.
Plugin removal leaves the separately installed runtime intact.
These actions do not delete retained stages. Review or drop them explicitly;
do not recursively delete the state root as an uninstall step.

## Maintenance

`backend.mjs` owns CLI translation, task identity, process supervision and output
bounds. `tools.mjs` owns the shared read/execute schema. Pi and DSH add only host
lifecycle/approval adaptation. `mcp.mjs` owns stdio JSON-RPC transport; it never
adds acceptance authority. Do not duplicate filesystem state, bypass native
locks, use `last`, retry commands automatically, or silently enable host execution.

Preserve this package's Apache-2.0 licensing. The integration was independently
implemented against the hosts' public contracts; no DeltaBox runtime is bundled.
