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
| DOC-PLUGINS-07 | npm automatically selects native dependencies; CLI needs no source path; uninstall preserves separate state | Exact documented npm/global CLI script against a loopback registry of packed fixtures; installed MCP doctor |
| DOC-PLUGINS-08 | Unsafe state permissions, replaced directories, foreign project/selector and path traversal fail closed | Adapter boundary tests; no fallback execution |
| DOC-PLUGINS-09 | Runtime deadlines fail without accepting output | Real native deadline |
| DOC-PLUGINS-10 | MCP shutdown does not accept or discard pending results | Real stdio shutdown, reopened adapter and native review |
| DOC-PLUGINS-11 | A project and the default durable stage can both live under HOME | Real native lifecycle in an isolated user HOME; private task HOME avoids inherited FUSE submounts |

The npm fixture verifies real package-manager installation and dependency
selection, not native file semantics or an already-published registry release.
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
only host registration and approvals. `runtime.mjs` locates npm's native optional
dependency; it never downloads or installs software during a tool invocation.
The npm `pvisor` launcher replaces itself with the native executable to preserve
terminal ownership, signals and exit codes. Keep ordinary CLI semantics intact.

## Distribution

Publish these three packages as one versioned cohort:

- `pvisor-agent-runtime-linux-x64`
- `pvisor-agent-runtime-darwin-arm64`
- `pvisor-agent-plugins`

The core's optional dependencies pin both platform packages exactly to its own
version. `runtime.json` declares the core runtime base version and supported
platform mapping. It does **not** select the old stable GitHub release by version
number: stable 0.3.1 lacks the required selective apply interface.

`agent-plugins.yml` reuses `distribution.yml` to build and accept both native
platform sets from the plugin's exact commit. An isolated distribution version
records the build number and commit. The existing pipeline verifies wheels,
native binaries, firmware provenance and corresponding sources.

The prepare job downloads only that workflow's accepted artifact set, writes
`runtime-provenance.json`, and runs:

```sh
npm run release:prepare --prefix plugins -- "$PWD/native"
```

The assembler requires matching commit provenance, validates SHA256SUMS and the
exact regular-file archive layout, and retains all native companions, licenses
and firmware metadata. It never downloads `latest` or weakens safety to support
an old CLI. The package test requires `apply --path --dry-run --json` support.
All npm installation checks use packed tarballs, not linked source directories.

GitHub Release `plugins-vX.Y.Z` retains the native distributions, checksums and
corresponding firmware source archives before npm publication. npm package
metadata links back to that release. Do not publish binaries without their
corresponding source availability. npm integrity and provenance cover the
platform tarballs users actually install.

## First publication

1. Confirm ownership of the three npm names. Authenticate with `npm login` on
   the maintainer's machine; never paste a token in chat or commit it.
2. Review both native acceptance results and the plugin package tests.
3. Publish the verified platform tarballs **before** the adapter tarball. Use
   `npm publish FILE.tgz --access public`. A missing optional package must not be
   mistaken for a complete release.
4. Configure npm Trusted Publisher for all three packages:
   repository `DeepLink-org/pvisor`, workflow `agent-plugins.yml`, environment
   `npm`. Configure that GitHub environment's required reviewers.
5. Future `plugins-vX.Y.Z` tag runs publish with OIDC and npm provenance.
   `workflow_dispatch` defaults to build-only; publishing requires explicitly
   selecting its publish input. Tag creation/publication requires maintainer
   authorization; passing tests does not authorize landing on main.
6. Verify installs from npm using Pi and DSH's managers and a disposable Codex
   home. Check the remote marketplace at the published tag. Then remove the
   first-release-pending notice from user docs.

Do not run `npm publish` when authentication is missing, claim a dry run is a
publication, or document an unpublished name as already installable. Retry an
incomplete release by inspecting the existing immutable package versions and
artifact hashes; never overwrite a published version or silently retag it.
