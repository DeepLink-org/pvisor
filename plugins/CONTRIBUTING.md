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
| DOC-PLUGINS-03 | Reject apply first, approve later; drop discards remaining candidates without another approval and preserves accepted files | Pi/DSH approval counts and native file state |
| DOC-PLUGINS-04 | Nonzero task exit retains candidates without retry or publication | Native CLI, reopened adapter and failed status |
| DOC-PLUGINS-05 | A fresh spawn cannot see another Session's pending changes | Two native Sessions |
| DOC-PLUGINS-06 | Host conflicts refuse writeback without destroying host edits | Native preview, apply, review and drop |
| DOC-PLUGINS-07 | npm installs only the plugin; host PATH selects an external runtime; uninstall preserves runtime and state | Packed plugin installed through a loopback registry; documented CLI check and installed MCP doctor against an external fixture |
| DOC-PLUGINS-08 | Unsafe state permissions, replaced directories, foreign project/selector and path traversal fail closed | Adapter boundary tests; no fallback execution |
| DOC-PLUGINS-09 | Runtime deadlines fail without accepting output | Real native deadline |
| DOC-PLUGINS-10 | MCP shutdown does not accept or discard pending results | Real stdio shutdown, reopened adapter and native review |
| DOC-PLUGINS-11 | A project and the default durable stage can both live under HOME | Real native lifecycle in an isolated user HOME; private task HOME avoids inherited FUSE submounts |
| Output | Capture stdout/stderr on success/failure and bound large output separately from CLI diagnostics | Native completed task and nonzero exit; UTF-8, invalid/oversized/nonterminal/foreign review fixtures |
| Rust offline | Read-only real toolchain, private Cargo state, project-vendored dependencies, no host writes before apply | Native cargo test with a vendored registry fixture and a non-truncating denied write-open of rustc |
| Experiments and cleanup | Named fanout, selector discovery, explicit drop without extra approval, partial refusal | Native DSH fanout and batch lifecycle; Pi/DSH no-prompt and cancellation fixtures |
| Workspace fork | Independent child files, native lineage/isolation/deadline inheritance, new HOME/Cargo state; no fallback for unsupported source | Pi/DSH/MCP native forks, Rust offline fork, native one-second source timeout and portable ownership/state guards |
| Artifact export | Explicit paths to a new operator-root child, native manifest, no acceptance or consumption | Pi/DSH/MCP failed-task exports, documented JSON cases, existing-link protection, unsupported symlink/applied/dropped refusals, invalid root/name/response fixtures |

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

### DSH monitor acceptance

The browser extension targets DSH **0.2.2-rc.1** (Node >=24.9). Old alpha tool
compiler acceptance is not browser compatibility. Install a disposable full
DSH, React and the packed plugin, without modifying the user's DSH profile:

```sh
mkdir -p plugins/dist/npm
npm pack ./plugins --ignore-scripts --pack-destination plugins/dist/npm
npm install --prefix plugins/node_modules/monitor-host --ignore-scripts \
  --no-audit --no-fund --package-lock=false \
  @deepseek-ai/dsh@0.2.2-rc.1 react@18.3.1 react-test-renderer@18.3.1 \
  "$PWD/plugins/dist/npm/pvisor-agent-plugins-0.1.12.tgz"
PVISOR_PLUGIN_BINARY=/absolute/path/to/pvisor \
  npm run test:monitor --prefix plugins -- "$PWD/plugins/node_modules/monitor-host/node_modules"
```

This boots an isolated loopback DSH Web instance with a disposable HOME,
telemetry disabled, no provider credentials and no model calls. It checks the
packed plugin's Host activation, client boot graph/bundle delivery, anonymous
and foreign-Origin refusal, authenticated real CLI task/change/output reads,
then real React component selection and rendering. The startup token and
browser cookie stay in memory. It stops the server and discards only its own
test task. It is not interactive browser, Electron, or the user's installed
DSH acceptance.

The Node component check uses DOM-equivalent fixtures for DSH's shell-seeded
styled Button/Pill/Tag atoms; it does not verify browser CSS or screenshots.
Use actual DSH Web to evaluate the visual integration in light/dark themes.
Creation-time sorting happens before pagination, using adapter timestamps for
new records and labeled record birthtime/mtime estimates for legacy records.
Default `pvisor_tasks` ID pagination remains compatible; the monitor always
requests `order: "newest"`. Confirmed history filtering is page-local only,
never a reason to hide unknown outcomes or silently delete retained evidence.

Task pages validate the runtime once and read up to four native statuses
concurrently, with a two-second transport deadline per row. A slow or invalid
read becomes an explicit unknown row; request cancellation still aborts the
whole observation. This does not terminate or retry the workload.
Foreground loading and background sampling are distinct. No timestamp-only
replies, busy-state toggles or redundant details should be published from
background reads. Preserve the selected selector across list membership changes.
Portable contracts cover quiet unchanged reads and off-page selection; the
native suite observes a real task in `running` before it completes.

`file-review.mjs` derives display-only per-file diffs from the public native
review manifest and text. Match only canonical project paths and validate hunk
lengths, so header-looking file content cannot start another file. Do not read
private upper trees, invent line totals for binary/skipped/truncated inputs,
or use derived counts/text as acceptance authority. Keep non-UTF-8/control
names available in inventory without guessing text framing. Native acceptance
covers added/modified/deleted files, directories, binary data and actual budget
truncation. The browser caps rendered file/diff rows independently.
Human titles and command previews are deliberately lossy display heuristics,
never shell inputs. Elapsed-time clocks update their own cells, not the list's
network-loading state. Preserve one primary run status and localized change
labels rather than showing execution and filesystem enums as peer status chips.

The integration check reads the packed `cordis.patch.yml` and resolves its
named package from a disposable owning tree. Do not replace it with an
absolute `dsh.mjs` fixture: that bypassed the 0.1.7 browser discovery bug.
DSH 0.2.2-rc.1 discovers package-root Loader names, not named subpaths such as
`pvisor-agent-plugins/dsh`; keep the root export and bundle row aligned.

`dsh-monitor.mjs` owns one GET-only authenticated Connection Fetch route;
browser-selected Session ids resolve projects from live/persisted headers.
Keep Backend ownership checks and never add arbitrary cwd, flags, file reads
or mutation passthrough. Four active reads and a 25-second request budget bound
native subprocess work. Connection's exact-route contract admits only owner
Peers; do not substitute an unauthenticated webserver route.

`dsh-client.js` is the source and shipped classic-script artifact for DSH's
`window.__ModuleLoader__.load({id, factory})` protocol, exported at `./client`.
It uses the shell's React instance and public Sidebar tab/slot registrations;
no bundler, CSS/DOM injection or npm install script is needed. Pure polling
contracts run in the portable suite. Keep visibility/session cancellation,
late-response rejection, explicit stale errors, pagination, directory counts
and display limits; do not conflate current candidate inventory with historical
output. The first UI is Chinese and read-only; no automatic task/run/apply/drop.

The explicit terminal action is an exception to read-only presentation, not a
new mutation route: GET `view=terminal` only prepares validated literal argv.
The browser opens a fresh DSH builtin terminal through `webTerminals` and
`sidebarRight`, selects a discovered Bash, waits for a writable connection and
sends one quoted `builtin exec -- ...; builtin exit`. Never replay input on
reconnect, change the remembered shell, silently resume, or substitute a host
shell when native Session exec is unavailable. Startup capability is rechecked
by the owner; only its acknowledgement prints the native connected banner.
The action needs the updated runtime and a newly started Linux Host namespace
Session. Portable terminal-service fixtures do not replace real DSH UI acceptance.

`backend.mjs` translates to native commands and owns adapter identity and process
supervision. `tools.mjs` is the shared model-facing schema. Host adapters own
only host registration and approvals. `runtime.mjs` selects an operator-specified
absolute CLI path or `pvisor` from the host PATH and probes required CLI
capabilities before use. It ignores empty and relative PATH entries and never
downloads or installs software. Homebrew/venv symlinks are supported. Invalid
overrides and incompatible selected runtimes never fall back. The npm package
must not provide a `pvisor` launcher or native runtime dependencies.

`execution.mjs` owns operator-only environment selection: default `auto`,
VM-required `vm`, explicit safe `host`. Only literal fallback authorization
permits `auto` to choose Host after a valid negative native preflight.
Invalid configuration/probe output and post-launch failures never trigger
downgrade. Keep image/firmware installation and host permission changes outside
the adapter. `vm-check` creates an empty platform VM and reports firmware
discovery separately; neither is guest boot or workflow certification.
Rootfs must be explicit and disjoint from project/state. Host Rust shares and
Host workspace forks must not silently cross this execution policy.
Portable tests simulate VM selection/failure; native Host tests explicitly
choose `host` rather than weakening the new default. Run real VM acceptance
only with a prepared guest rootfs and compatible firmware.

Captured task output is read through native `status --review --json`, which
validates the owning runtime's completion contract. Never read completion.json,
Bundle files or upper trees directly from the adapter. Bound review collection
to 8 MiB and displayed task streams to 256 KiB each. Preserve unavailable output
and uncertain CLI outcomes instead of fabricating empty successful completion.

`environment.mjs` owns the operator's optional Rust offline profile; runtime
mount semantics remain in the native CLI. `fanout.mjs` is a bounded independent
task driver, shared by model tools and the operator CLI. It does not grant
acceptance rights, interpret results as correctness, or retry unknown tasks.
Batch drop retains native lifecycle admission and task records; never implement
it by removing state directories. An already-applied refusal is normalized only
after native terminal/empty/applied status and a clean empty dry-run agree on
source identity and generation. Never match error text or suppress pending
recovery, partial apply or unknown-state failures. List pages are current observations, not
snapshot-isolated scans; opaque UUID cursors may miss tasks inserted earlier
in sort order between requests.

Fork reuses the task execution/output pipeline but only calls native
`fork --state workspace` with an explicit source/child stage and command.
Never reconstruct policy from private files or duplicate checkpoint contents.
The source's native timeout cannot be overridden by this CLI; use only a bounded
630-second transport watchdog and state that it is not a new workload deadline.

Export calls native `export`, not filesystem copies. `PVISOR_PLUGIN_EXPORT_DIR`
is operator-only, existing, owned 0700 and disjoint from project/state/toolchain.
Output is one new child name, never arbitrary host paths; explicit relative
selections and native no-overwrite/lock/type/completion checks remain in charge.
Keep the exact destination on cancellation or invalid/truncated responses.
Do not clean or retry uncertain artifacts, consume source candidates or silently
broaden policy for unsupported backends. Native manifests are returned as-is.

`presentation.mjs` formats native apply plans; it never invents changes or test
results. Preview reads JSON and scoped text through separate dry runs, then
compares native plan identities before presenting confirmation. This is not a
reservation across the human decision or an atomic apply guarantee. Keep native
JSON available to callers, bound display excerpts, fail closed on malformed or
changed plans, and keep Agent explanation separate from verified facts. DSH
requires a short intent summary; compact approval text must remain readable when
the host collapses whitespace and must not repeat raw diff or launcher banners.
Normal/blocked summaries, hostile/long display data, no-ops and collection races
have portable fixtures; real native cases assert only selected paths/diffs appear.

Native tests require rustup with the configured Rust toolchain. Set
`PVISOR_PLUGIN_TEST_RUST_TOOLCHAIN` to select its actual root; by default the
test uses `rustup which rustc`. Native Rust acceptance uses a tiny generated
vendored registry dependency, not network downloads or the operator's Cargo cache.
Record real platform and project coverage separately from this fixture.

The new native output, Rust and fanout cases were exercised on Linux x86_64
with the released pVisor 0.3.2 CLI and Rust 1.98.0. Pi/DSH compiler checks cover
the nested fanout schema as well as existing tools. These are not DSH Desktop
UI, cloud-model, macOS Rust profile or Pulsing project acceptance results.

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
