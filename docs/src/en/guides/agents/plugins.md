---
description: Install pVisor through Homebrew or pip, add Pi, DSH and Codex plugins, and use native result handoff.
---

# Pi, DSH and Codex plugins

Call pVisor staged tasks from your Agent, then hand off results through the native `spawn → status/review → apply/drop` workflow. Install the runtime through Homebrew or pip; npm distributes only the Agent adapters.

!!! note "Upgrade from 0.1.0"
    Use plugin 0.1.1 or newer for Homebrew/pip runtimes. The published 0.1.0 package still requires obsolete npm runtime packages; reinstalling it does not fix that dependency. Remote marketplace tags require separate publication; npm publication alone does not make those refs available.

The output, offline Rust, fanout and batch-cleanup features started in plugin 0.1.2; later test builds improved drop and apply presentation. Version 0.1.6 added native Host workspace fork, scoped artifact export and structured review. Version 0.1.8 fixed DSH monitor discovery, and 0.1.11 fixed refresh churn. The current 0.1.12 test build presents task runs and per-file inline diffs. Registry install commands require publication; successful source tests are not publication evidence.

## Install {#install}

Prepare Node.js 22.19+, your Agent host and a separate pVisor installation through [Homebrew or pip](../../start/installation.md). Host staging supports Linux x64 and macOS arm64 and requires the [Host staging prerequisites](../../start/installation.md#local-prerequisites). The plugin does not install a runtime, elevate privileges to install FUSE, enable extensions or change namespace policy.

Launch the Agent host with `pvisor` on its PATH. Desktop apps may not inherit your terminal PATH; set `PVISOR_PLUGIN_BINARY` in the host environment to the absolute installed CLI path if needed. The plugin ignores empty and relative PATH entries and checks required CLI flags before work. Incompatible runtimes fail with upgrade instructions; older releases such as 0.3.1 without selective apply are insufficient. Plugin and runtime versions are independent.

Choose the hosts you actually use:

```sh
# Pi
pi install npm:pvisor-agent-plugins@0.1.12

# DSH: install only the profile you use
dsh plugin --profile web add pvisor-agent-plugins@0.1.12
dsh plugin --profile desktop add pvisor-agent-plugins@0.1.12

# Codex CLI: run from your project
codex plugin marketplace add DeepLink-org/pvisor --ref plugins-v0.1.12
codex plugin add pvisor@pvisor
codex mcp add pvisor -- npx --yes --package pvisor-agent-plugins@0.1.12 pvisor-agent-mcp --project "$PWD"
```

DSH Desktop also accepts `pvisor-agent-plugins` in **Plugins → Add plugin**. Install only the profile you use.

The Codex marketplace installs the workflow skill; native `codex mcp add` binds tools to your current project. `npx` installs only the pinned plugin without a global install; the runtime must already be installed. Clients without plugin management can use MCP alone. Bundled MCP can start in the plugin cache, so that cwd cannot identify the user's project.

Start a new session in the target project. Call `pvisor_doctor` and check the binary path, project and runtime version; it checks required CLI capabilities but does not attest FUSE mounting or isolation. Pi uses the call context's cwd; DSH uses the initiating Agent's cwd. MCP refuses startup without an explicit project instead of falling back to the plugin directory.

For an actual sandbox startup check, call `pvisor_doctor` with `{"probe":true}`. This runs a small safe task and returns its selector under `probe`; inspect or explicitly drop it like any other task. It does not auto-clean candidates or certify isolation beyond successful startup.

## Execute a task {#spawn}

Ask the Agent to call `pvisor_spawn`, for example with:

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

The tool calls native `pvisor spawn` with the operator-selected environment, `--safe`, deny-all networking, captured stdio and a deadline. Source builds default to VM-first, with no unauthorized Host downgrade; configure the environment below before running examples. It does not load personal Agent presets. The deadline is 1–600 seconds, defaulting to 120. Each spawn creates an independent Session: put dependent edits/tests in one call or explicitly fork a stopped retained candidate. Fresh sibling spawns cannot see unaccepted results.

Keep the returned absolute stage selector. Plugin tools `pvisor_status`, `pvisor_review` and `pvisor_apply_preview` take the same selector as the native CLI; there is no plugin ID to translate. Never use `last`. Failures, cancellation and unknown completion can leave candidates; do not retry or clean up automatically.

### Read captured output {#output}

Spawn returns task `stdout`/`stderr` separately from launcher diagnostics in `cli`. `exit_code` remains the CLI exit code; `task_exit_code` and `task_state` describe the completed task. Task streams are limited to 256 KiB each, and `truncated` includes native capture truncation. The adapter collects output through validated native `status --review --json` with an 8 MiB budget, not by reading private completion records.

`output_status: unavailable` means there is no validated terminal output, or the read exceeded its budget. Check `output_error` and `cli`; do not interpret missing output as success. Interrupted owners are not queried automatically. Use `pvisor_output` with the same selector after checking the task, without executing it again.

## Prepare offline Rust tasks {#rust-offline}

An operator can expose an installed Rust toolchain read-only while retaining deny-all networking and private build state. Before starting the Agent host, select the actual toolchain directory:

```sh
export PVISOR_PLUGIN_RUST_TOOLCHAIN="$(dirname "$(dirname "$(rustup which rustc)")")"
```

Doctor reports `environment.kind: rust-offline` and the selected toolchain. The root must contain real `bin/cargo`, `bin/rustc` and `bin/rustdoc`, outside the project, plugin state and private temporary paths. A rustup shim directory or the entire `~/.cargo` is not a toolchain root. This operator-controlled directory is an explicit read grant; do not select one containing secrets.

Prepare dependencies in a trusted terminal, outside task execution, with a checked-in Cargo.lock:

```sh
cargo vendor --locked --versioned-dirs vendor > vendor-source-config.toml
```

Merge the generated source sections into `.cargo/config.toml` without overwriting existing settings. Keep `vendor/`, the lockfile and the source configuration available in the project. Vendoring may need network access during preparation; the plugin does not fetch dependencies or run rustup installation.

Ask `pvisor_spawn` to run argv `["cargo","test","--offline","--locked"]`. The profile provides direct rustc/rustdoc paths, sets `CARGO_NET_OFFLINE=true`, and places `CARGO_HOME` and `CARGO_TARGET_DIR` in each task's private HOME. It does not inherit the user's Cargo registry, credentials, config, Rust wrappers or rustup override. System linkers, C headers, native libraries and build-script dependencies must still be prepared. A successful small vendored fixture is not evidence that every Rust project builds.

## Collect experiments and clean up {#experiments}

Call `pvisor_fanout` with a named experiment plan:

```json
{
  "concurrency": 2,
  "tasks": [
    {"name": "a", "argv": ["sh", "-c", "echo candidate-a > result.txt; echo a"]},
    {"name": "b", "argv": ["sh", "-c", "echo candidate-b > result.txt; echo b"]}
  ]
}
```

Each entry is an independent native Session. Limits are 1–8 tasks, 1–4 concurrent tasks and the same 1–600 second per-task deadline. The report retains names, exact selectors, bounded output, failures and elapsed time. `elapsed_order` lists completed tasks by elapsed time; it is not a correctness ranking or controlled performance measurement. Review and test the candidates before choosing one. Unknown completion is never retried; cancellation leaves unstarted entries marked `not_started`.

An operator can save the same JSON as `experiments.json` and use the CLI driver:

```sh
npx --yes --package pvisor-agent-plugins@0.1.12 pvisor-agent-fanout \
  --project "$PWD" --input experiments.json
```

Use plugin tools instead when a nested host terminal cannot safely launch the native CLI. The driver only executes and collects; it never accepts or discards candidates.

`pvisor_tasks` lists this project's adapter tasks with native lifecycle/overlay status and selectors. It accepts `limit` (1–50, default 20) and the returned `next_cursor`. Pages are current observations, not a snapshot. Malformed/foreign adapter records are not listed, and inserted tasks may require a new scan.

DSH's `pvisor_drop_batch` accepts `{"selectors":["/exact/stage-a","/exact/stage-b"]}`. Pi offers `/pvisor drop-batch` with the same JSON. Drop runs without additional confirmation: all 1–50 explicit selectors are checked before any mutation, then native drop admits each operation again. Per-task outcomes are `dropped`, `already_applied`, `refused`, `unknown` or `not_attempted`; prior drops are not rolled back. A terminal fully applied Session is a successful `already_applied` no-op only after native status and a clean empty dry-run confirm there are no remaining candidates or pending apply. This is not rollback or deletion; the original refusal code is retained as `native_exit_code`. Partial apply, unknown/live/suspended state, pending recovery or invalid evidence never become benign results. Codex MCP exposes no batch mutation tool.

No idle TTL or recursive directory removal deletes pending work. Native drop handles candidates; adapter task records remain. Failed/uncertain runs can retain a private `scratch_home`, separate from project results. This is not a complete disk-space collector.

## Fork a retained candidate {#fork}

With a stopped parent that staged `branch.txt`, call `pvisor_fork`:

```json
{
  "selector": "PARENT_SELECTOR",
  "argv": ["sh", "-eu", "-c", "cat branch.txt; printf 'child\\n' > branch.txt"]
}
```

Use the exact selector returned by spawn; the placeholder is not a lookup alias. The native CLI creates a workspace checkpoint, restores files and conflict preimages to an independent child stage and runs the explicit command. The parent remains reviewable and the project is unchanged until apply. The response includes `parent_selector`, the new child selector, captured output and native `lineage`; subsequent status/review/apply/drop use the child selector.

The source must belong to this project/plugin and have validated terminal completion and retained staged data. Public evidence must confirm an enforced Host filesystem/network boundary and explicit deny-all capability. Failed/cancelled candidates can be useful inputs. Live, unknown, suspended, fully applied and dropped sources are refused. Native workspace policy reconstruction also rejects unsupported VM/container/Gateway or prepared-workspace sources, rather than starting a fresh task with weaker defaults.

Native safety, read grants and timeout are inherited. Fork exposes no timeout, executor, share or checkpoint override. It gets fresh private HOME and offline Rust state when configured; changing the operator profile does not add grants to the source's inherited policy. Start a new parent after changing toolchain/profile. The transport watchdog is 630 seconds, while the native source deadline still applies.

This branches file proposals, not process memory, Agent conversation or immutable copies of every host lower. Parent/child stages share the plugin state filesystem. Do not automatically rerun uncertain forks; a child record/selector can remain after failure.

## Export artifacts without acceptance {#export}

An operator must first configure an existing private artifact root outside the project, plugin state and configured toolchain:

```sh
mkdir -m 700 "$HOME/pvisor-artifacts"
export PVISOR_PLUGIN_EXPORT_DIR="$HOME/pvisor-artifacts"
```

Do not overwrite an existing root or change its permissions blindly; verify ownership and mode `0700` before reuse. Restart the Agent host with this environment. Doctor reports the canonical root; unset configuration disables export.

For a retained child containing `branch.txt`, call `pvisor_export`:

```json
{
  "selector": "CHILD_SELECTOR",
  "paths": ["branch.txt"],
  "output": "child-artifact"
}
```

The plugin invokes native export with explicit project-relative paths and `--output ROOT/child-artifact`. `output` must be one new directory name of 1–128 letters/digits/`._-`, starting with a letter or digit; no absolute paths, separators, `..` traversal or implicit all selection. Names share the configured root across projects, so choose unique names. Native validation refuses destinations overlapping project/stage/storage/authority and never replaces existing files, directories or links.

Linux directory uppers only. Native completion validation and locks admit retained completed/failed/cancelled tasks; active, unknown, applied and dropped sources are refused. Symlinks, special files, type replacements and opaque dependencies are unsupported. These errors do not trigger a fallback copy or relaxed policy.

Regular contents are under `files/`; native `manifest.json` records logical metadata and informational directory/deletion entries. Export does not apply deletions, reproduce all metadata, include unselected lower files/dependencies or create a complete patch/merged snapshot. The source remains available for review, fork, export, apply or drop as its lifecycle permits. Export evidence before full apply/drop removes retained data.

`export_status: published` means only that the artifact directory was published. `manifest` is the native response, with source outcome, entries, deletions and `partial_results`; source failure does not make successful evidence extraction an error. `destination` and `manifest_path` locate the result. There is no additional approval, acceptance, automatic source cleanup or upload.

Export has a 120-second transport watchdog. A cancelled process or lost/invalid/truncated reply can race publication: `export_status: unconfirmed` retains the exact destination. Inspect it before deciding what to do; do not automatically retry, overwrite or remove uncertain artifacts.

## Bind one Agent to an instance {#agent-binding}

A task selector does not redirect an Agent's built-in shell, reads or patches. The task plugin does not currently provide a persistent per-Agent binding.

For one staged interactive Agent process, launch the Agent itself inside a native Session and let its tools operate there, as in the [whole-Codex workflow](codex.md#run-codex). Keep authentication/network policy explicit and do not nest these task plugins inside that Session.

For repeated external commands in one long-lived VM, Linux [container staged binds](../../reference/cli.md#container-staged-binds) support `run`/`exec` followed by review/apply/drop after stopping the container. That container lifecycle is separate from native Session continuation. Binding one DSH subagent to such an instance requires a host adapter that routes its filesystem and shell tools; the current plugin cannot enforce that by storing an instance ID.

## Review and accept {#handoff}

### Monitor tasks in DSH Web

Plugin 0.1.12 provides a read-only **pVisor** page in the right Sidebar guide
(also reachable through its tab strip's add control). Restart DSH Web and
refresh the page after installing. The browser extension targets DSH
**0.2.2-rc.1**; check `dsh --version`. Earlier alpha tool tests do not prove
UI compatibility, and Desktop/Electron acceptance is not claimed.

Upgrade from 0.1.7: its bundle mounted the named `/dsh` subpath, which DSH
0.2.2-rc.1 skips during browser-module discovery. Tools can work while the
monitor is absent. Version 0.1.8 mounts the package root instead, with `/dsh`
retained as an alias. Stop/restart DSH Web and refresh the browser after updating.

The panel shows the current Session project's task pages, separate execution
and stage states, structured candidate entries and on-demand terminal output.
It samples sequentially one second after a read for pages with live/running/
unknown tasks, otherwise after two seconds. Background reads do not toggle
loading or clear rows; unchanged data, including timestamp-only replies,
publishes no UI update. Selected tasks remain open if pushed off-page.
This is sampling, not push or guaranteed observation of very short runs.
Hidden tabs, background Sessions and hidden browser pages pause sampling and abort reads.
Errors explicitly label retained observations as stale. Each page contains up
to ten tasks with previous/next navigation; page counts are not project-wide
totals or a frozen snapshot. Changes count files and directories separately;
at most 200 entries are displayed with an explicit limit notice. Use
`pvisor_review` for the complete bounded inventory. Captured output is bounded,
historical and terminal-only, not live streaming. The initial UI uses Chinese
labels and provides no run/apply/drop/fork/export controls or automatic cleanup.
It uses the existing Backend through DSH's owner-authenticated API channel,
not a separate unauthenticated server or browser filesystem access.

The panel uses DSH's shared Button/Pill/Tag controls and theme tokens.
Tasks sort newest creation first before pagination, with native command
excerpts and times. New records carry adapter creation time; legacy record
birthtime (or mtime fallback) is an estimate marked `≈`. `pvisor_tasks` keeps
its ID-order default; request `order: "newest"` and retain that order across
cursor requests to use this ordering.

Run rows use a display name when supplied or a command-derived title,
a secondary monospace command without added argv quotes, one primary status,
relative Chinese time and elapsed duration. Display heuristics never authorize
command execution. Precise dates use Chinese 24-hour formatting and the
browser's time zone in collapsed details.

**待处理** hides confirmed cleared/applied history on the current page;
**全部** shows it. Filtering is page-local; older retained tasks may be on
another page. The project name opens collapsed project/sorting/history
information. Empty pages offer **查看全部**; pagination shows only available
directions. **已清理** means native drop cleared candidate
upper/work data while task metadata and execution evidence remain, not that
candidate files are still retained. This is not a disk-usage measurement or
automatic garbage collection. Clicking a task opens **文件变更**
and **终态输出**. File selection shows inline diffs alongside the list
(stacked in narrow panes), not in a separate task-level Diff tab. Chinese
change labels and `+N −M` come from validated native hunks; directories have no
invented size, and binary/skipped/truncated inputs retain unknown (`null`)
line counts. Control/non-UTF-8 names retain their inventory without guessed
text framing. At most 200 file rows and 2000 diff lines render, with notices.
Diff is a separate bounded observation; cleared candidates cannot be recovered
from an empty stage.

Terminal attachment is not implemented. A stopped task is not a persistent
shell; entering a candidate interactively requires a new isolated workspace
branch with a managed PTY, not a Host shell or direct stage-directory access.

`pvisor_review` returns `review_status`, `changes[]`, `counts`, `in_scope`,
`outside_scope`, `unclassified`, native `handoff` and `review_context`.
Entries preserve native `path`, `path_bytes` when present, `kind`, `old_type`,
`new_type`, `size_bytes` and `mode`; paths are relative to `filesystem.target`
(the plugin project). `counts.total` counts entries, not files: `file`,
`directory`, `symlink`, `other` and `unknown` are separate counts. Deletions
use `old_type`; other entries use `new_type`. Native unsupported project entries
remain visible in `handoff.unsupported_project_changes`; review does not grant
apply/export permission.

Use `{"selector":"STAGE_FROM_RESULT","include_diff":false}` for structured
inventory only. By default, bounded text diff remains in `stdout`.
`diff_status: "separate_observation"` means JSON and text were read separately,
not as one frozen snapshot; `truncated` or `unavailable` must not be treated as a
complete diff. Current inventory is refreshed after apply/drop; execution
evidence remains historical. If native review is unknown, malformed, truncated
or fails, `review_status` is `unavailable` and `changes`/`counts` are `null`,
not an empty successful review. Unavailable scope classification produces
`in_scope: null` and `outside_scope: null`, not zero. Review does not run tests
or replace a fresh conflict-checked apply preview.

Use the native CLI already installed through Homebrew or pip:

```sh
pvisor --version
```

Save the returned selector in the terminal variable `stage`, then:

```sh
pvisor status "$stage" --json
pvisor review "$stage" --diff
pvisor apply "$stage" --path accepted.txt --dry-run --diff
pvisor apply "$stage" --path accepted.txt
pvisor drop "$stage"
```

Check accepted files and rerun the relevant tests. Drop only unwanted remaining candidates; skip it after accepting everything. Native ownership, completion receipts and conflict checks remain authoritative; plugins do not bypass a running owner's stage lock. Drop does not undo applied files or external effects. See [review and apply](../review-apply.md).

Pi also offers commands; apply requires interactive confirmation, while drop does not:

```text
/pvisor status STAGE_FROM_RESULT
/pvisor review STAGE_FROM_RESULT
/pvisor apply {"selector":"STAGE_FROM_RESULT","paths":["accepted.txt"]}
/pvisor drop STAGE_FROM_RESULT
```

DSH's `pvisor_apply` requires Harness `Allow once` after a native preview. Rejection, cancellation or unavailable approval cannot publish files. Single and batch drop do not request approval, including Pi commands without an interactive UI. They discard explicitly selected candidates directly, while retaining project ownership, selector and native lifecycle checks. No automatic drop is introduced. Codex MCP exposes neither publication nor discard: the user runs the native terminal commands above. Unrelated host permissions remain unchanged; a skill cannot constrain an Agent that already has host shell access.

Before preview, the adapter calls native status to reconcile Session control state, then `apply --dry-run`. It never creates internal lock files directly or accepts project files during this preflight.

### Understand the apply summary {#apply-summary}

The approval card is a short paragraph that remains readable when the host collapses whitespace: Agent intent (marked unverified), project, native change count, at most three paths, remaining counts and a task identifier. It contains no raw diff, native banner or full state-directory selector. Longer change sets explicitly direct you to the full preview for omitted paths.

The separate preview result retains project/stage, requested paths, actual selected changes by kind, required dependencies, remaining counts and scoped text detail. Unselected inventory is not presented as an approved change set. A blocked preview lists candidate inventory and blockers and states that this request wrote no project files; no approval is requested.

The adapter reads native JSON and scoped text dry runs separately because runtime 0.3.2 rejects `--json` with `--diff`. It rechecks the plan after collecting text. A changed plan, malformed response, mismatched source/project or failed/truncated detail collection cannot open an approval prompt. This checks display consistency during collection, not a transaction or a reservation while the user decides. Execution still performs native conflict checks.

Machine-readable JSON remains in `stdout`; human display uses `summary`. `diff`, `preview_state` and `approval_ready` are separate fields. Large inventories and excerpts explicitly say when display is truncated. Missing text diff is not evidence that content is unchanged, and preview does not run tests.

DSH's `pvisor_apply` requires a nonempty `summary` string (1–512 bytes) explaining what the selected changes do and why, preferably in the user's language, after reviewing the diff. Do not paste logs or diff into this field. Pi operator commands accept it optionally. The card labels it as unverified Agent intent, not native facts or test evidence; the text cannot change selection or authorize execution. Empty matching plans return an informational no-op instead of asking for approval.

## Configuration and scope {#scope}

| Environment variable | Default | Meaning |
| --- | --- | --- |
| `PVISOR_PLUGIN_BINARY` | `pvisor` on the host's PATH | Absolute installed CLI path; an invalid override never falls back |
| `PVISOR_PLUGIN_STATE_DIR` | `~/.local/state/pvisor/agent-plugins/v1` | Private task records and stages outside the project |
| `PVISOR_PLUGIN_PROJECT` | Unset | Explicit project for manually launched MCP; prefer native `codex mcp add ... --project DIR`; Pi and DSH use their call context |
| `PVISOR_PLUGIN_EXECUTION` | `auto` | Prefer VM; `vm` requires VM, `host` explicitly retains safe Host execution |
| `PVISOR_PLUGIN_ALLOW_HOST_FALLBACK` | `false` | Literal `true` authorizes `auto` fallback only before task launch |
| `PVISOR_PLUGIN_VM_ROOTFS` | Unset | Explicit prepared Linux rootfs and tools; owned/non-writable-by-others, disjoint from project/state; host `/` forbidden |
| `PVISOR_PLUGIN_RUST_TOOLCHAIN` | Unset | Operator-selected actual toolchain root; read-only access with private offline Cargo state |
| `PVISOR_PLUGIN_EXPORT_DIR` | Unset (export disabled) | Existing owned `0700` artifact root outside project/state/toolchain; model chooses only a new child name |

The state directory must be owned by the current user with mode 0700. Tasks cannot read another project's adapter records. Task records do not persist argv or credentials. Native runtime output and records retain their existing rules; avoid printing secrets.

Each task gets a fresh private HOME and XDG state paths, without host HOME login state, user configuration or caches. Durable stages stay in the configured state directory. Failed/interrupted execution may retain scratch HOME; it is not a candidate project result. Prepare tools that depend on user HOME configuration separately instead of disabling isolation.

### VM-first policy {#execution-policy}

The source implementation requires a runtime with `vm-check --json`; this is
not a claim about the published 0.3.2 binary. Configure a prepared Linux
rootfs before starting the Agent host:

```sh
pvisor vm-check --json
env PVISOR_PLUGIN_VM_ROOTFS=/absolute/prepared-linux-rootfs dsh web
```

Linux preflight opens KVM, checks its API version and creates an empty VM using
the Agent host's permissions. Apple Silicon creates/destroys an HVF VM; this
path has not been validated on this Linux host. Firmware discovery is separate:
`bundled_unloaded` is not loaded-library or guest-boot evidence.
Missing virtualization, firmware or rootfs blocks default execution. A malformed,
failed or interrupted preflight requires correction/upgrade, not downgrade.
The plugin never changes device permissions, downloads images or uses host `/`
as an implicit rootfs. Prepare guest tools and dependencies inside the rootfs.

To permit Host fallback, persist this operator setting in the Agent host's
launch configuration; Agent tool arguments cannot change it:

```sh
env PVISOR_PLUGIN_ALLOW_HOST_FALLBACK=true dsh web
```

Only `auto` honors this permission after a valid negative preflight.
`PVISOR_PLUGIN_EXECUTION=vm` never falls back; `PVISOR_PLUGIN_EXECUTION=host`
explicitly retains the previous safe Host workflow and works without the new
VM preflight command. The same variables apply to Pi and Codex/MCP.
VM launch/boot failure never causes a Host retry. Doctor distinguishes
preflight from its optional real staged probe; task rows show the selected
backend and authorized fallback, not proof a failed task booted.
Host workspace fork requires explicit Host policy or authorized `auto` fallback.
VM workspace fork, VM Session terminal attachment and Container policy are not
exposed by these Host-only tools. The Rust share profile also requires Host;
VM Rust tools belong inside the prepared rootfs.

These task adapters do not override native filesystem, Shell, patch, background-job or other MCP tools. Arbitrary network/executor overrides, task stdin, Agent-wide tool routing and per-tool savepoints are not model inputs. For whole-Codex staging, use the existing [managed Codex launch](codex.md#run-codex), without nesting task plugins.

## Validate and maintain {#verify}

Call `pvisor_doctor` in a disposable project and run the two-file task above. Check that neither file appears on the host early. Review both, apply only `accepted.txt`, then drop the other candidate; the accepted file must remain. Test both approval and rejection in Pi/DSH. Keep safety enabled after startup errors and fix the reported system prerequisites.

Host fixtures do not establish real Pi UI, DSH Web/Desktop, Electron or Codex plugin-loader acceptance. Record Node, pVisor and Agent versions, tool catalog, project and approval behavior during host verification. These checks do not establish cloud-model end-to-end or approval-free acceptance.

Review pending tasks before upgrading. Update the plugin through the host's package manager and the runtime separately through Homebrew or pip, then restart and run Doctor to check compatibility. Never replace a runtime during a running task. Update Codex's pinned marketplace ref and npx version together. Remove `npm:pvisor-agent-plugins`, `pvisor-agent-plugins` or `pvisor@pvisor` through the respective host manager. Remove the separate MCP registration with `codex mcp remove pvisor`. Plugin removal leaves the runtime and candidates intact; do not recursively clear the state directory.
