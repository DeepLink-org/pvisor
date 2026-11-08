# pVisor Agent plugins

Run a bounded task from Pi, DeepSeek Harness (DSH), or Codex, inspect its staged
files, and accept selected results. These are **task adapters**, not replacements
for the host's built-in filesystem, terminal, or background-job tools. Each
`pvisor_spawn` starts a new native Session; later calls do not continue that Session.
Put dependent edits and tests in the same `argv` invocation.

First-release status: these registry and remote-marketplace entrypoints are
prepared but are not live until the npm packages and marketplace commit are
published. Do not treat the commands below as evidence of publication.

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
pi install npm:pvisor-agent-plugins
```

Start a new Pi session in the target project. Ask it to call `pvisor_doctor`,
then `pvisor_spawn`. The extension adds tools without overriding Pi's native tools.
For whole-Agent staging, launch Pi itself through `pvisor spawn --safe` instead;
do not nest task plugins inside that outer Session.

### DSH Web and Desktop

```sh
dsh plugin --profile web add pvisor-agent-plugins
dsh plugin --profile desktop add pvisor-agent-plugins
```

Install only the profile you use, then restart it. Desktop also accepts
`pvisor-agent-plugins` in **Plugins → Add plugin**. The package
uses Cordis's functional plugin interface and `dsh-tools.defineTool`; its peers
target DSH 0.2.x. It does not disable existing providers or change permission
presets. Check that `pvisor_doctor` appears in the actual tool catalog.

### Codex CLI

Register the remote marketplace and bind the tools from your project directory:

```sh
codex plugin marketplace add DeepLink-org/pvisor --ref plugins-v0.1.0
codex plugin add pvisor@pvisor
codex mcp add pvisor -- npx --yes --package pvisor-agent-plugins@0.1.0 pvisor-agent-mcp --project "$PWD"
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

Run commands always use Host execution, `--safe`, `--no-agent-defaults`,
deny-all networking, captured stdio and a runtime deadline (1–600 seconds,
default 120). Personal Agent presets are not loaded. No credentials are
explicitly passed into the task. Each task gets a fresh private HOME and XDG
state paths; host login state, user configuration and HOME caches are not inherited.
Tools installed only inside the original HOME may therefore be unavailable;
use system-installed tools or project-local dependencies that work offline.
Durable workspace stages stay in the configured state directory. Scratch HOME
may remain after a failed/interrupted execution; it is not an accepted project
result. Networked tasks, VM selection, interactive
input, PTYs, and live Session continuation are not exposed in this first version.
Do not remove safety flags to make a failed task work.

The private task record contains only its schema, project and task ID; it does
not persist command arguments or credential values. Runtime-owned records and
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

`pvisor_spawn` returns the exact stage selector, CLI exit code and bounded
stdout/stderr. The adapter captures noninteractive workload pipes (stdin is EOF)
using native `--stdio inherit`; native `--stdio capture` instead retains output
only in the Session record and does not forward it. Adapter output includes CLI
diagnostics and is limited to 256 KiB per stream; inspect `truncated` before
treating it as complete. Workload output is not also recorded by native stdio
capture. A nonzero exit, cancellation, or lost completion can leave
candidates. Do not automatically rerun an uncertain task. Use that exact selector;
the adapters never use `last`.

If the MCP connection disappears before the spawn response arrives, this release
has no task-list/recovery tool. An operator can inspect the private state directory
configured above: each UUID task directory has a `task.json` with its canonical
`project` and matching `task` UUID. Identify the intended task explicitly, then
use that directory's absolute `stage` path with native `pvisor status ... --json`
and review, or with the reopened plugin's exact-selector tools. Several records
may belong to the same project; do not guess from directory order or use `last`.
A record can precede native startup, so an absent stage is not proof that it is
safe to rerun. Do not edit records or invent completion receipts.

Closing MCP input cancels active supervised commands. Losing the response pipe
also cancels them once an output write detects the disconnect. Killing the MCP
server abruptly cannot run that cleanup: native work may continue until its
own completion or runtime deadline. Reconcile status before acceptance; live or
uncertain work is not approval to apply or discard candidates.

MCP keeps reading cancellation messages when a client stops reading responses.
Completed replies share an internal 8 MiB serialized UTF-8 output backlog budget,
including JSON escaping and protocol errors; the 32 active-request limit alone
does not bound completed replies. Exceeding this budget closes input, cancels
active supervised commands and reports the overload on stderr. Already queued
output gets at most one second to drain before the output pipe is destroyed.
A forced close can leave a partial final frame or undelivered results. Treat
those outcomes as uncertain, reconcile exact task selectors, and never rerun or
accept automatically. This bounds the server's queued transport bytes, not total
process memory, native execution output, or the client's buffers.

| Tool | Effect |
| --- | --- |
| `pvisor_doctor` | Check required CLI capabilities and show binary path, project, runtime version and adapter scope; it does not test FUSE or certify isolation |
| `pvisor_spawn` | Call native `pvisor spawn` for one new staged task and retain its Session |
| `pvisor_status` | Query the exact Session's JSON status |
| `pvisor_review` | Show the bounded project diff |
| `pvisor_apply_preview` | Call native `pvisor apply --dry-run` for explicit relative paths |

File acceptance happens after the native task exits. Runtime ownership,
completion receipts and conflict checks remain authoritative. A successful CLI
exit is not proof of a successful user task or sufficient evidence to accept it.

### Pi decisions

Pi commands use its interactive confirmation dialog:

```text
/pvisor status STAGE_FROM_RESULT
/pvisor review STAGE_FROM_RESULT
/pvisor apply {"selector":"STAGE_FROM_RESULT","paths":["accepted.txt"]}
/pvisor drop STAGE_FROM_RESULT
```

Apply first shows a bounded change summary and asks for confirmation. The summary
separates requested paths, native selected changes, additional required changes
(also applied), and other staged candidates (not applied). It reports action,
entry type, reported size/mode when available, remaining counts and blockers.
Long lists and labels explicitly disclose omitted details. Native preview does
not identify binary files, previous modes, semantic intent or test outcomes;
the Agent should explain purpose, observed tests and risks separately.
Detailed native `review --diff` and exact-selection `apply --dry-run --diff`
commands remain available; raw preview stdout is preserved on tool responses,
with an additional structured `summary`. A blocked, malformed, interrupted or
truncated preview cannot open an apply confirmation. Native execution still
rechecks conflicts; the summary grants no authority or completion evidence.
Drop asks separately.
With no interactive UI, mutations are refused. Tool calls do not expose apply
or drop, so model-generated JSON cannot stand in for user confirmation.

### DSH decisions

DSH additionally exposes `pvisor_apply` (`selector`, `paths`) and `pvisor_drop`
(`selector`). Each requires an initiating Agent and an explicit `allowed-once`
answer from the host approval service. Cancellation, no approval answerer,
rejection, and approval errors do not mutate the stage or project. The approval
names the project, stage and selection; apply includes the runtime preview.

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

### Opt-in long-lived native checks

The ordinary native suite skips `test/long-lived.native.mjs` unless
`PVISOR_PLUGIN_LONG_LIVED=1`. On Linux with a compatible native binary, run
one case per bounded invocation:

```sh
PVISOR_PLUGIN_LONG_LIVED=1 PVISOR_PLUGIN_LONG_LIVED_CASE='long-lived cancel' \
  mise run test-agent-plugins-long-lived
PVISOR_PLUGIN_LONG_LIVED=1 PVISOR_PLUGIN_LONG_LIVED_CASE='long-lived deadline' \
  mise run test-agent-plugins-long-lived
```

Set `PVISOR_PLUGIN_BINARY` to the installed CLI and
`PVISOR_PLUGIN_TEST_OUTPUT_DIR` to an owned durable evidence directory outside
the disposable project. Each case takes about nine minutes: six minutes of
kernel-timer waits with minute heartbeats, then three minutes of paused Pi mock
approval over the real adapter and native stage. Cancellation uses a 540-second
native limit; the deadline case uses 360 seconds. These are below the plugin's
600-second input maximum. The adapter supervisor adds 30 seconds to the runtime
limit; the fixture run bound is 650 seconds, the test bound 700 seconds. An
external harness should allow 850 seconds plus at most 30 seconds kill grace.
Harness interruption is not evidence that the runtime deadline fired.

The checks sample Linux available memory, free disk and load every minute and
abort owned work if the campaign limits (6 GiB, 20 GiB, load1 8) are crossed.
They exercise live mutation refusal, an inherited stdout holder, exact-selector
recovery, bounded output and approval summaries together. They do not launch an
actual GUI host or certify arbitrary descendant teardown. Successful fixtures
are reconciled, dropped and removed; uncertain fixtures retain their diagnosed
root paths. Save runner stdout/stderr outside those fixtures if retaining the
execution evidence is required. Never automatically rerun an interrupted task.

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

`backend.mjs` owns CLI translation and task identity. `process.mjs` owns bounded
output and process supervision for both runtime probes and task commands.
Cancellation also interrupts capability checks; a stalled probe has a ten-second
deadline, followed by at most five seconds each for termination escalation and
output draining. Forced termination does not certify descendant teardown.
`tools.mjs` owns the shared read/execute schema. Pi and DSH add only host
lifecycle/approval adaptation. `mcp.mjs` owns stdio JSON-RPC transport; it never
adds acceptance authority. Do not duplicate filesystem state, bypass native
locks, use `last`, retry commands automatically, or silently enable host execution.

Preserve this package's Apache-2.0 licensing. The integration was independently
implemented against the hosts' public contracts; no DeltaBox runtime is bundled.
