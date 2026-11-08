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

### Opt-in paced endurance

`paced-endurance.native.mjs` is skipped by ordinary native CI unless
`PVISOR_PLUGIN_PACED_ENDURANCE=1`. It models distinct tiny coding/review cycles,
not throughput: successful partial acceptance, exit7 and retained review,
fixture-only host conflict restoration, cancellation after observed readiness,
fresh adapter views of one exact task, and empty/no-op apply. Round6 separately
covers two retained task identities; this fixture uses only one Session at a time.
The six stories are followed by two seeded observation-dependent choices.

After obtaining the sole campaign TEST/BUILD slot, create an owned durable
evidence directory and TMPDIR, then run each segment once, sequentially:

```sh
PVISOR_PLUGIN_PACED_ENDURANCE=1 PVISOR_PLUGIN_PACED_SEGMENT=1 \
  PVISOR_PLUGIN_PACED_SEED=1401 PVISOR_PLUGIN_BINARY=/absolute/installed/pvisor \
  PVISOR_PLUGIN_TEST_OUTPUT_DIR=/absolute/owned/evidence \
  TMPDIR=/absolute/owned/tmp mise run test-agent-plugins-paced
```

Repeat with segments2,3,4 only after inspecting the preceding segment. Each
segment has eight serial Node subtests; each fixture's after hook reconciles
terminal/nonlive authority and drop (or applied-empty) before the next cycle.
Every inter-cycle gap is75 real seconds, with before/midpoint/after resource
and owned-process/mount/stage observations. The last campaign cycle has no
trailing gap. Across32 cycles, intended idle is2325 seconds (38m45s), plus
actual active setup/edit/test/review/cleanup time, reported separately.
Each segment has a820s Node bound and the task enforces850s plus30s kill grace,
nice10, two Cargo jobs and one test/math thread. The task passes its exact
pattern and concurrency to Node, never as trailing file arguments.

Guards stop on available RAM<6GiB, disk<20GiB, load1>8, owned evidence>16MiB,
or a non-quiescent cycle boundary. JSONL evidence is exclusive-create and
bounded to4MiB per segment; do not automatically rerun uncertain segments.
Host `/proc` observations record birth ticks and report permission denials and
races. They cover self/observable owned process mount views, not every namespace;
host RAM deltas alone are not leak evidence. No observer sends host signals.
Uncertain fixture authority preserves its root. No global cleanup is performed.
The current opt-in runner refuses launches after the2026-11-08 09:04UTC campaign
deadline; a future campaign must deliberately update that policy.

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
| DOC-PLUGINS-12 | Cancelling an active task retains its exact selector and staged candidates without publication | Real native cancellation after a staged write; reopened status, review, read-only preview and drop |
| DOC-PLUGINS-13 | Active MCP EOF, truncated-frame EOF, abrupt server death and client pipe loss retain candidates and refuse live mutations | Real native stdio faults before the spawn response, explicit private-record selector discovery, reopened reconciliation/review/preview/drop; separate fake-runtime test verifies output loss cancels even with input still open |
| DOC-PLUGINS-14 | Host edits after preview still conflict at apply; changing caller input or active project does not redirect approval | Deterministically paused Pi/DSH approval callbacks, two real native projects/stages, external host edit, refusal and explicit recovery with a new approval |
| DOC-PLUGINS-15 | Rejected, cancelled, unavailable or failed approvals preserve candidates | Pi false/throwing confirmation and DSH closed outcomes/service error/aborted signal, real native file absence and reopened review/preview after each refusal |

The npm fixture verifies real package-manager installation and external runtime
selection, not Homebrew/pip installation, native file semantics or a published registry release.
The native suite never substitutes a fake executable or skips unavailable
staging. CI retains native case directories on failure.

Approval-race fixtures mock host registration and approval callbacks, not a real
interactive Pi or DSH UI. Pi receives a command string and parses its own input.
The DSH fixture models detached, frozen JSON arguments as described by the
[upstream tool-author contract](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/cookbook/adding-a-tool.md).
That upstream `master` documentation is not a revalidation of the pinned host
release. Mutating a direct `registerDsh` callback's borrowed object outside that
dispatch contract is not evidence of a normal-host approval bypass.

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

`backend.mjs` translates to native commands and owns adapter identity.
`process.mjs` supervises all runtime subprocesses, including capability and
version probes, with bounded output, cancellation, escalation and stream draining.
Task launch uses native `--stdio inherit` with the supervisor's pipes, not a
terminal; stdin receives EOF. Native `--stdio capture` is record-only and would
hide workload output from tool results. The adapter retains at most 256 KiB
per output stream and reports truncation; native stdio does not persist a
second copy. `coding-task.native.mjs` checks real offline Python unittest
stdout/stderr, nonzero results, private HOME/XDG, generated bytecode/log disposal,
and nested-Git index isolation through atomic save, rename, chmod and selective
acceptance. Its default interpreter is `/usr/bin/python3`; operators can set
`PVISOR_PLUGIN_TEST_PYTHON` to an offline interpreter accessible under `--safe`.
An interpreter installed only under the original HOME may be inaccessible.
`descendants.native.mjs` checks Linux native inherited stdout/stderr and stdin
EOF with a background child or detached `setsid` grandchild, both after exit7
and user cancellation. Its at-most-three-process workloads each arm independent
45-second alarms. Host `/proc` birth identities and exact owned argv tokens
observe teardown with bracketed birth/UID rechecks and readiness-birth matching;
denied or unstable observations fail closed and preserve the fixture root.
Terminal observation must precede the40s voluntary exit, and any survivor
fails even if it exits during the subsequent safety-only wait. Namespace PIDs
are never used for host signals. These cases
check exact-selector terminal status, retained review, rejected Pi mock approval
and no retry/publication. They do not certify arbitrary descendant containment
on other executors or platforms, or exercise MCP transport/backpressure.
`tools.mjs` is the shared model-facing schema. Host adapters own
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
