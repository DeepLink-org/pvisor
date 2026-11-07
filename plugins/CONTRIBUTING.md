# Maintain and publish the Agent plugins

The user entrypoints are registry packages and the remote Codex marketplace.
Source paths, test globs and build directories belong here, not in onboarding.
The first npm release is not published until a maintainer authorizes npm access.

## Develop

Node.js 22.19+ is required. From the repository root:

```sh
mise run test-agent-plugins
PVISOR_PLUGIN_BINARY="$PWD/target/debug/pvisor" mise run test-agent-plugins-smoke
```

The first command runs adapter, process, MCP and distribution fixtures. The second
uses the real CLI and disposable files to check staging, review, selective apply
and drop. It fails rather than skips when staging prerequisites are missing.
Source installations may use `pi install ./plugins` or a DSH local package path;
these are contributor workflows, not public installation instructions.

Run all real-CLI documentation cases with:

```sh
PVISOR_PLUGIN_BINARY="$PWD/target/debug/pvisor" mise run test-agent-plugins-native
```

### Documentation coverage

The tests read the canonical English/Chinese guide's executable JSON, native
handoff commands and Pi commands directly. They fail on bilingual drift or a
missing example; they do not approve semantic specifications or edit review ledgers.

| Case | User-visible claim | Automated evidence |
| --- | --- | --- |
| DOC-PLUGINS-01 | Doctor, staged two-file task, status and review | Pi/DSH adapters and real stdio MCP against the native CLI |
| DOC-PLUGINS-02 | Preview does not publish; selective apply; drop preserves accepted files | All three routes; exact documented CLI handoff script and Pi commands |
| DOC-PLUGINS-03 | Reject first, approve later; rejected drop preserves remaining candidates | Pi confirmation and DSH approval contracts against native file state |
| DOC-PLUGINS-04 | Nonzero task exit retains candidates without retry or publication | Native CLI, reopened adapter and failed status |
| DOC-PLUGINS-05 | A fresh spawn cannot see another Session's pending changes | Two native Sessions |
| DOC-PLUGINS-06 | Host conflicts refuse writeback without destroying host edits | Native preview, apply, review and drop |
| DOC-PLUGINS-07 | npm installs only the plugin; host PATH selects an external runtime; uninstall preserves runtime and state | Packed plugin installed through a loopback registry; documented CLI check and installed MCP doctor against an external fixture |
| DOC-PLUGINS-08 | Unsafe state permissions, replaced directories, foreign project/selector and path traversal fail closed | Adapter boundary tests; no fallback execution |
| DOC-PLUGINS-09 | Runtime deadlines fail without accepting output | Real native deadline |
| DOC-PLUGINS-10 | MCP shutdown does not accept or discard pending results | Real stdio shutdown, reopened adapter and native review |
| DOC-PLUGINS-11 | A project and the default durable stage can both live under HOME | Real native lifecycle in an isolated user HOME; private task HOME avoids inherited FUSE submounts |

The npm fixture verifies real package-manager installation and external runtime
selection, not Homebrew/pip installation, native file semantics or a published registry release.
The native suite never substitutes a fake executable or skips unavailable
staging. CI retains native case directories on failure.

Ordinary PR CI runs portable contracts on Linux/macOS with Node 22.19.0 and 24,
actual pinned Pi/DSH loader checks, and the strict native suite in the Linux CLI
job. Real Pi UI, DSH Web/Desktop/Electron, remote marketplace availability and
cloud-model approval behavior remain separate acceptance checks; fixture success
must not be reported as those outcomes.

For actual host compiler/loader checks, install disposable dependencies separately
and pass their package roots:

```sh
npm run test:hosts --prefix plugins -- /path/to/pi-coding-agent /path/to/dsh-tools
```

Tested contracts: Pi 0.79.3 extension loading and argument validation; DSH
`dsh-tools` 0.2.1-alpha.2's `defineTool` compiler. Run real Pi UI and DSH
Web/Desktop approval checks separately. No model credentials are needed for
these compiler/loader checks.

`backend.mjs` translates to native commands and owns adapter identity and process
supervision. `tools.mjs` is the shared model-facing schema. Host adapters own
only host registration and approvals. `runtime.mjs` selects an operator-specified
absolute CLI path or `pvisor` from the host PATH and probes required CLI
capabilities before use. It ignores empty and relative PATH entries and never
downloads or installs software. Homebrew/venv symlinks are supported. Invalid
overrides and incompatible selected runtimes never fall back. The npm package
must not provide a `pvisor` launcher or native runtime dependencies.

## Distribution

Publish only `pvisor-agent-plugins` to npm. Users install the native runtime
separately through Homebrew or pip (`pvisor-sandbox`), following the project
installation guide. Runtime releases retain their existing native distribution,
firmware/source and acceptance workflows; the plugin workflow does not rebuild
or publish them.

Plugin and runtime versions are independent. Compatibility is checked by the
required CLI flags in `runtime.mjs`, including `apply --path --dry-run --json`.
Stable 0.3.1 lacks selective apply and cannot be used. Before publishing, verify
that Homebrew or pip offers a compatible runtime and record its version in real
host acceptance results. Capability probes do not establish staging readiness.

`agent-plugins.yml` tests adapters and packed-plugin installation, then retains
one npm tarball in `pvisor-agent-plugin-release`. Installation checks use packed
tarballs, not linked source directories. `workflow_dispatch` with `publish=false`
builds and tests without publication. There are no npm native packages, runtime
downloaders, installation scripts or same-name CLI launchers.

## First publication

1. Confirm ownership of `pvisor-agent-plugins`. Authenticate with `npm login` on
   the maintainer's machine; never paste a token in chat or commit it.
2. Review plugin package tests and native/interactive host acceptance with a
   separately installed compatible runtime.
3. Download the verified workflow artifact and publish only its plugin tarball:
   `npm publish pvisor-agent-plugins-X.Y.Z.tgz --access public`.
4. Configure npm Trusted Publisher for the plugin:
   repository `DeepLink-org/pvisor`, workflow `agent-plugins.yml`, environment
   `npm`. Configure that GitHub environment's required reviewers.
5. Future `plugins-vX.Y.Z` tag runs publish with OIDC and npm provenance.
   `workflow_dispatch` defaults to build-only; publishing requires explicitly
   selecting its publish input. Tag creation/publication requires maintainer
   authorization; passing tests does not authorize landing on main.
6. Verify installs from npm using Pi and DSH's managers and a disposable Codex
   home, with a Homebrew/pip runtime available in the host environment. Verify
   missing/incompatible runtime errors and Desktop PATH handling. Check the
   remote marketplace at the published tag. Then remove the first-release-pending
   notice from user docs.

Do not run `npm publish` when authentication is missing, claim a dry run is a
publication, or document an unpublished name as already installable. Retry an
incomplete release by inspecting the existing immutable package versions and
artifact hashes; never overwrite a published version or silently retag it.
