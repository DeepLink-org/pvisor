---
description: Install pVisor through Homebrew or pip, add Pi, DSH and Codex plugins, and use native result handoff.
---

# Pi, DSH and Codex plugins

Call pVisor staged tasks from your Agent, then hand off results through the native `spawn → status/review → apply/drop` workflow. Install the runtime through Homebrew or pip; npm distributes only the Agent adapters.

!!! note "First publication pending"
    The npm packages and remote marketplace commit are not published yet. The commands below are the post-publication user entrypoints, not evidence that registry installation already works. Remove this notice only after publication authorization and release acceptance.

## Install {#install}

Prepare Node.js 22.19+, your Agent host and a separate pVisor installation through [Homebrew or pip](../../start/installation.md). Host staging supports Linux x64 and macOS arm64 and requires the [Host staging prerequisites](../../start/installation.md#local-prerequisites). The plugin does not install a runtime, elevate privileges to install FUSE, enable extensions or change namespace policy.

Launch the Agent host with `pvisor` on its PATH. Desktop apps may not inherit your terminal PATH; set `PVISOR_PLUGIN_BINARY` in the host environment to the absolute installed CLI path if needed. The plugin ignores empty and relative PATH entries and checks required CLI flags before work. Incompatible runtimes fail with upgrade instructions; older releases such as 0.3.1 without selective apply are insufficient. Plugin and runtime versions are independent.

Choose the hosts you actually use:

```sh
# Pi
pi install npm:pvisor-agent-plugins

# DSH: install only the profile you use
dsh plugin --profile web add pvisor-agent-plugins
dsh plugin --profile desktop add pvisor-agent-plugins

# Codex CLI: run from your project
codex plugin marketplace add DeepLink-org/pvisor --ref plugins-v0.1.0
codex plugin add pvisor@pvisor
codex mcp add pvisor -- npx --yes --package pvisor-agent-plugins@0.1.0 pvisor-agent-mcp --project "$PWD"
```

DSH Desktop also accepts `pvisor-agent-plugins` in **Plugins → Add plugin**. Install only the profile you use.

The Codex marketplace installs the workflow skill; native `codex mcp add` binds tools to your current project. `npx` installs only the pinned plugin without a global install; the runtime must already be installed. Clients without plugin management can use MCP alone. Bundled MCP can start in the plugin cache, so that cwd cannot identify the user's project.

Start a new session in the target project. Call `pvisor_doctor` and check the binary path, project and runtime version; it checks required CLI capabilities but does not attest FUSE mounting or isolation. Pi uses the call context's cwd; DSH uses the initiating Agent's cwd. MCP refuses startup without an explicit project instead of falling back to the plugin directory.

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

The tool calls native `pvisor spawn` with Host execution, `--safe`, deny-all networking, captured stdio and a deadline. It does not load personal Agent presets. The deadline is 1–600 seconds, defaulting to 120. Each call creates a new Session: put dependent edits and tests in one call; later tasks cannot see unaccepted results.

Keep the returned absolute stage selector. Plugin tools `pvisor_status`, `pvisor_review` and `pvisor_apply_preview` take the same selector as the native CLI; there is no plugin ID to translate. Never use `last`. Failures, cancellation and unknown completion can leave candidates; do not retry or clean up automatically.

## Review and accept {#handoff}

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

Pi also offers user commands with interactive confirmation before each mutation:

```text
/pvisor status STAGE_FROM_RESULT
/pvisor review STAGE_FROM_RESULT
/pvisor apply {"selector":"STAGE_FROM_RESULT","paths":["accepted.txt"]}
/pvisor drop STAGE_FROM_RESULT
```

DSH's `pvisor_apply` and `pvisor_drop` reuse Harness approval and require `Allow once`. Rejection, cancellation or unavailable approval cannot publish changes. Apply shows a native preview before asking. Codex MCP exposes neither publication nor discard: the user runs the native terminal commands above. Keep the host's approvals; a skill cannot constrain an Agent that already has host shell access.

Before preview, the adapter calls native status to reconcile Session control state, then `apply --dry-run`. It never creates internal lock files directly or accepts project files during this preflight.

## Configuration and scope {#scope}

| Environment variable | Default | Meaning |
| --- | --- | --- |
| `PVISOR_PLUGIN_BINARY` | `pvisor` on the host's PATH | Absolute installed CLI path; an invalid override never falls back |
| `PVISOR_PLUGIN_STATE_DIR` | `~/.local/state/pvisor/agent-plugins/v1` | Private task records and stages outside the project |
| `PVISOR_PLUGIN_PROJECT` | Unset | Explicit project for manually launched MCP; prefer native `codex mcp add ... --project DIR`; Pi and DSH use their call context |

The state directory must be owned by the current user with mode 0700. Tasks cannot read another project's adapter records. Task records do not persist argv or credentials. Native runtime output and records retain their existing rules; avoid printing secrets.

Each task gets a fresh private HOME and XDG state paths, without host HOME login state, user configuration or caches. Durable stages stay in the configured state directory. Failed/interrupted execution may retain scratch HOME; it is not a candidate project result. Prepare tools that depend on user HOME configuration separately instead of disabling isolation.

These task adapters do not override native filesystem, Shell, patch, background-job or other MCP tools. Networked tasks, VM selection, PTYs, stdin, live Session continuation and per-tool savepoints are not exposed yet. For whole-Codex staging, use the existing [managed Codex launch](codex.md#run-codex), without nesting task plugins.

## Validate and maintain {#verify}

Call `pvisor_doctor` in a disposable project and run the two-file task above. Check that neither file appears on the host early. Review both, apply only `accepted.txt`, then drop the other candidate; the accepted file must remain. Test both approval and rejection in Pi/DSH. Keep safety enabled after startup errors and fix the reported system prerequisites.

Host fixtures do not establish real Pi UI, DSH Web/Desktop, Electron or Codex plugin-loader acceptance. Record Node, pVisor and Agent versions, tool catalog, project and approval behavior during host verification. These checks do not establish cloud-model end-to-end or approval-free acceptance.

Review pending tasks before upgrading. Update the plugin through the host's package manager and the runtime separately through Homebrew or pip, then restart and run Doctor to check compatibility. Never replace a runtime during a running task. Update Codex's pinned marketplace ref and npx version together. Remove `npm:pvisor-agent-plugins`, `pvisor-agent-plugins` or `pvisor@pvisor` through the respective host manager. Remove the separate MCP registration with `codex mcp remove pvisor`. Plugin removal leaves the runtime and candidates intact; do not recursively clear the state directory.
