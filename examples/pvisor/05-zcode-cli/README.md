# Normal ZCode CLI commands under pVisor

Install a complete ZCode CLI/TUI distribution first, then use its normal command:

```bash
zcode
pvisor run -- zcode
pvisor run -- zcode --prompt 'Explain this project'
```

Use a complete official CLI/TUI installation, rather than the Electron desktop
executable. This adapter was exercised with ZCode CLI 0.16.9 (upstream commit
`872ad96`). Install Node and the CLI outside the project being staged.

## Runtime and state permissions

Create `~/.config/pvisor/agents/zcode.toml` (or
`$XDG_CONFIG_HOME/pvisor/agents/zcode.toml`) with existing absolute paths:

```toml
[[run.filesystem]]
path = "/absolute/path/to/zcode-installation"
access = "read"

[[run.filesystem]]
path = "/absolute/path/to/node-installation"
access = "read"

[[run.filesystem]]
path = "/absolute/home/.zcode"
access = "read_write"

[overlayfs]
commit = "manual"
```

TOML paths are literal: replace the placeholders; `~` and `$HOME` are not
expanded. Preserve ZCode's existing login/provider configuration. The profile
loads only from trusted personal configuration, never from the project.

This profile enables `[overlayfs]` with `commit = "manual"`, so
`pvisor run -- zcode` stages project writes without an extra flag. Generic
pVisor runs do not enable staging merely by granting runtime paths. **Writes to explicitly granted persistent state
are immediate and outside `pvisor apply` / `pvisor drop`.** pVisor prints this
exception when it starts. Never grant the whole home directory. Grants that
overlap the project or Run storage are rejected, as are overlapping read-only
and writable grants. On Linux read-only grants under the sandbox's private
writable `/tmp` are rejected; install runtimes outside `/tmp`.

The generic configuration is available to other agents too:

```toml
[[run.filesystem]]
path = '/absolute/runtime/path'
access = 'read'

[[run.filesystem]]
path = '/absolute/persistent/state'
access = 'read_write'
```

Both paths must already exist. The corresponding one-shot options are
`--fs-read PATH` and `--fs-write PATH`. These settings currently support the
host executor. JSON `--spec` uses its existing filesystem capabilities instead.
Profiles are loaded from personal configuration, never from project files.
`--no-config` disables a saved profile; it does not automatically discover the
runtime permissions that profile supplied.

## BigModel Coding Plan capture

For the official ZCode CLI 0.16.9 with its existing BigModel Individual Coding
Plan login, add this to the same personal `agents/zcode.toml` profile:

```toml
[gateway]
profile = "zcode-bigmodel"
zcode_builtin_config = "/absolute/ZCode/apps/zcode-cli/packages/cli/dist/provider/zcode-builtin.json"
level = "dialogue"

[overlaynet]
mode = "proxy"
```

Continue using `pvisor run -- zcode`. The host executor binds a Run-local
listener and gives ZCode a read-only snapshot of the installed, non-secret
built-in provider catalog. Only `account:bigmodel-individual-coding-plan` is
redirected to Gateway; its provider identity and ZCode-managed credentials
are retained. The installed catalog, login store and saved endpoint are not
rewritten. Personal model selections remain in ZCode's existing personal
configuration. Built-in catalog refresh is disabled for this Run so it cannot
replace the temporary endpoint. An unknown catalog schema, missing provider,
or changed provider protocol/endpoint fails explicitly.

Model requests enter the local Gateway/OverlayNet listener and Gateway
forwards them to BigModel. This example requires direct outbound access to the
model endpoint; external proxy chaining is outside this change. Native Anthropic Messages/SSE is preserved. Gateway records
model content according to `level`; other HTTPS traffic stays a CONNECT tunnel
without content decryption. This profile covers the individual account provider,
not custom API-key providers, team plans, or other providers selected in the TUI.
VM/container adaptation is not yet supported.

Even without this Gateway profile, enabling OverlayNet proxy mode for a direct
`zcode` command now injects `ZCODE_HTTP_PROXY` and `ZCODE_NO_PROXY`. These
Run-specific values replace passed host values. As elsewhere in host proxy
mode, direct sockets remain outside cooperative network enforcement.

## Deterministic integration test

This example calls **`pvisor run ... -- zcode ...`** directly. No CLI code is
copied into the project, and no special test launcher sits between pVisor and
ZCode. A local OpenAI-compatible SSE server supplies deterministic responses;
the installed ZCode executes the real `Write` and `Bash` tools.

The test creates a disposable provider configuration and persistent state
outside the project, and selects them using ZCode's environment variables.
It never needs a real API key or replaces the user's provider configuration.
The test's temporary personal pVisor profile grants only its own state and the
installed runtime. The model URL explicitly targets the test Gateway; normal
HTTPS providers do not automatically gain content capture from this test.

From the repository root, set paths for your installation:

```bash
export PVISOR_BIN="$(command -v pvisor)"
export ZCODE_NODE="$(command -v node)"
export ZCODE_RUNTIME_ROOT="/absolute/path/to/ZCode"
export ZCODE_ENTRY="$ZCODE_RUNTIME_ROOT/apps/zcode-cli/packages/cli/dist/zcode.cjs"
export ZCODE_BUILTIN_PROVIDER_CONFIG_FILE="$ZCODE_RUNTIME_ROOT/apps/zcode-cli/packages/cli/dist/provider/zcode-builtin.json"
export WORK_ROOT="$PWD/target/zcode-example"
bash examples/pvisor/test.sh 05-zcode-cli
```

The deterministic integration test requires Linux rootless isolation and FUSE3,
Python 3, jq, Bash, Node, and the installed `zcode` and updated `pvisor` commands.
It asserts actual filesystem enforcement instead of accepting a best-effort
fallback. If the host restricts user namespaces through AppArmor, use an
installation path already permitted by the administrator's policy.

On macOS the host executor uses macFUSE for staging and Seatbelt for write
restrictions; full-disk reads remain ambient. The `/proc` process-cleanup checks
in this integration script are Linux-only. Use the ordinary commands above
for macOS interactive testing, then inspect the Run with `pvisor review` and
choose `pvisor apply` or `pvisor drop`.

Each scenario recreates its own subdirectory under `WORK_ROOT`; use it only
for disposable data. Do not run tests concurrently against the same root.
The standard example runner still runs 01–04 unless 05 is selected explicitly.

A normal `zcode` baseline first confirms immediate file writes. Isolated-run
assertions cover streamed tool execution, untouched original files, apply and
drop in separate runs, persistent session state, a real Bash `sleep 120` child,
10-second timeout and no surviving observed processes. Expected result:

```text
RESULT example=zcode-cli tool_write=1 sse_requests=2 applied=1 dropped=1 state_persisted=1 normal_command=1 normal_baseline=1 timeout=passed survivors=0
```

Evidence is under `write/zcode-cli`, `drop/zcode-cli` and
`timeout/zcode-cli-timeout`: Run Bundles, Gateway events, mock requests and
runtime hashes. Top-level process snapshots and test logs record each scenario.

## Scope

The automatic Gateway profile targets the individual account provider in the
catalog schema above. API-key providers need an explicit provider endpoint and
Gateway route; this profile does not automatically redirect them. Plugin/MCP
execution, session replay and AgentCtl checkpoints are not covered by this
example. Host networking remains cooperative.
