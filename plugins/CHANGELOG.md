# Agent plugin changelog

## Working tree — VM-first operator policy

- Default to `auto`: preflight VM access and configured guest environment;
  refuse execution unless VM is ready or the operator explicitly authorized Host
  fallback. `vm` never falls back; `host` explicitly retains safe Host execution.
- Add native `vm-check --json`: Linux opens KVM, checks API and creates an empty
  VM; Apple Silicon creates/destroys an HVF VM. Report firmware discovery
  separately, with `boot_verified: false`.
- Require explicit prepared Linux rootfs; never use host `/` implicitly.
  No permission changes, image download or retry after user argv launch.
- Share policy across Pi/DSH/MCP/fanout; record/display the selected backend and
  fallback. Preserve source policy for existing tasks; Host forks require Host
  authorization. VM fork/Session terminals and Container policy remain unsupported.
- Breaking default change: older native binaries need an upgrade for preflight,
  or an operator must explicitly select `PVISOR_PLUGIN_EXECUTION=host`.
  Source tests do not imply npm/runtime publication or real guest boot acceptance.

## Working tree — live Session terminal

- Add an explicit DSH terminal action for running, exec-capable Linux Host
  Sessions. Use DSH's public terminal services and native `exec --session -it`;
  do not replay input, resume stopped tasks or substitute a host shell.
- Prefer optional lifecycle-only native status for task polling, without stage
  file/checkpoint/ledger traversal. Older runtimes retain the existing fallback.
- Detach the Linux sandbox's inherited host mount tree so a running sibling
  cannot pin another Session's FUSE teardown.
- Requires the updated native runtime and newly started Sessions. Native review
  pagination, global/current-Session attribution and stop controls remain out
  of this increment; no npm release is implied.

## 0.1.12 — Unreleased

- Present task runs with human titles, secondary command excerpts without added
  argv JSON quotes, one primary status, relative Chinese time and elapsed clocks.
  Keep precise 24-hour/time-zone dates in collapsed details.
- Retain existing fanout names as display metadata, never executable arguments.
- Replace the separate task Diff tab with per-file selection and inline text
  diffs, responsive side-by-side/stacked layout and validated `+N −M` counts.
  Localize change labels and distinguish directories, binary and skipped data.
- Derive file presentation only from public native review data. Validate hunk
  lengths and canonical paths; partial/ambiguous input never becomes `+0 −0`.
  Keep 200-file/2000-rendered-line caps with explicit notices.
- Acceptance adds hostile/header-looking contents, binary, deletion, actual
  truncation and human-title contracts. Browser CSS remains an acceptance gap;
  stop controls, terminal attachment and cross-Session/global lists are not
  introduced by this P0 presentation change.

## 0.1.11 — Local test build, not an npm release

- Read task status with bounded concurrency (four) and per-row deadlines (two
  seconds), validating the runtime once per page. One unresponsive owner becomes
  an unknown row instead of stalling all task discovery.
- Separate foreground refresh from silent background sampling. Unchanged
  content (including timestamp-only replies) keeps existing object identity
  and publishes no update. The refresh button keeps its label.
- Sample after one second for live/running/unknown rows, otherwise two seconds.
  This remains bounded sampling, not push or guaranteed observation of short runs.
- Avoid redundant detail reads; keep inspecting the selected task if new tasks
  push its row off-page. Preserve cancellation, scope checks and stale errors.
- Add a real CLI running-before-completion case and portable no-repaint,
  off-page-selection and slow-status regression coverage.

## 0.1.10 — Local test build, not an npm release

- Remove persistent workflow/safety/sorting/history explanations from the
  monitor. Keep a read-only badge, compact project disclosure and task controls.
- Replace empty-page paragraphs with a short state and a "查看全部" action.
  Show only available pagination actions. Keep errors and truncation warnings.
- Move detail provenance and interpretation notes into collapsed task information.

## 0.1.9 — Local test build, not an npm release

- Use DSH's shared Button/Pill/Tag atoms and theme tokens, compact command/time
  rows and a separate detail view instead of a UUID-heavy browser-default list.
- Sort monitor tasks by creation time before pagination. New adapter records
  carry creation timestamps; legacy record birthtime/mtime estimates are
  labeled. Add optional `pvisor_tasks` order without changing its ID default.
- Hide confirmed cleared/applied history on the current page by default,
  with a visible hidden count and an all-records switch. Keep unknown tasks.
  This changes presentation, not native cleanup or retention.
- Add task-level native text Diff beside candidate files and captured output.
  Diff is a separate bounded observation, not a frozen snapshot.
- No terminal attachment, automatic history deletion or mutation controls.
  Node rendering tests fixture the shell's styled atoms, not browser CSS.

## 0.1.8 — Local test build, not an npm release

- Fix the missing DSH monitor entry: mount `pvisor-agent-plugins` at its package
  root and export the Host plugin there. Keep `/dsh` as a compatibility alias.
  DSH 0.2.2-rc.1 skips named package subpaths during browser-module discovery.
- Correct integration acceptance to read the actual packed bundle patch and
  resolve its package name, rather than mounting an absolute `dsh.mjs` path.
  The corrected check reproduces 0.1.7's absent boot-graph entry.
- Upgrade/restart DSH Web and refresh the browser; no runtime upgrade, stage
  migration, approval change or npm publication is implied.

## 0.1.7 — Local test build with a browser-discovery defect

- Add a DSH Web right-Sidebar **pVisor** page using the public tab/slot protocol,
  targeting DSH 0.2.2-rc.1. No DOM injection, custom server port or install build.
- Read task pages, execution/stage states, structured candidate entries and
  terminal captured output through an owner-authenticated Connection Fetch
  route. Resolve project from the DSH Session; retain Backend selector checks.
- Poll only while visible, sequentially with cancellation and stale-response
  guards. Keep failures distinct from empty results. Display up to 200 changes
  with a limit notice; task pagination is not a project-wide summary.
- No task execution, acceptance, cleanup or other mutation controls in the UI.
  Existing tool approvals and Pi/MCP contracts remain unchanged.

Acceptance includes portable visibility/cancellation/boundary contracts, a
disposable full DSH 0.2.2-rc.1 Host with the packed plugin, served boot graph,
real browser module-loader import, authentication/Origin refusal, real Linux
CLI monitor reads and React component selection/rendering. No interactive
browser, Electron, macOS or Windows monitor acceptance is claimed.
The original Host fixture used an absolute path and did not verify the shipped
named bundle row. It therefore missed the frontend discovery failure fixed in
0.1.8; those earlier results do not establish a working 0.1.7 public install.

## 0.1.6 — Local test build, not an npm release

- Add structured `pvisor_review`: native `changes`, scope counts, typed entry
  counts, handoff and review context. `include_diff: false` skips text collection.
  Unavailable evidence is not an empty inventory; text diff is a separate read,
  not a frozen snapshot or acceptance check.
- Add `pvisor_fork` across Pi, DSH and MCP using native workspace fork, with a
  new child selector, output and native lineage. Require a confirmed stopped
  retained Host parent and explicit command; inherit native policy/deadline,
  use a fresh private task HOME and do not expose execution restore/passthrough.
- Add `pvisor_export` using the native non-consuming artifact contract.
  Require an operator-selected owned 0700 `PVISOR_PLUGIN_EXPORT_DIR`, explicit
  paths and one new child name. No arbitrary output paths, overwrite, acceptance
  or extra approval. Preserve the destination on unconfirmed publication.
- Report fork/export configuration and limits through Doctor.
- Accept an omitted optional native task exit code as unknown (`null`) when
  terminal output is otherwise validated, including fork deadline failures.

Native acceptance covers parent/child independence and isolation, fresh Rust
offline state, inherited deadlines, failed-task artifacts, no overwrite,
unsupported symlinks and refusal of applied/dropped sources. Real DSH UI,
macOS fork/export acceptance and general Windows Host staging are not claimed.

## 0.1.5 — Local test build, not an npm release

This release collects the locally tested 0.1.2–0.1.4 workflows and the compact
approval-card correction. It is not published merely because a local tarball exists.

- Return bounded task stdout/stderr separately from CLI diagnostics, with explicit
  task state, exit code and unavailable-output handling. Recover output through
  the exact selector without rerunning work.
- Add an explicit Doctor startup probe, project-scoped task discovery, named
  bounded fanout and an operator CLI driver. Elapsed order is informational,
  not correctness ranking or benchmark evidence.
- Add an operator-selected read-only Rust toolchain with private offline Cargo
  state and project-vendored dependencies. No shared host Cargo credentials/cache.
- Remove extra approval prompts for single/batch drop in DSH and Pi. Explicit
  selectors, cancellation, project ownership and native lifecycle checks remain.
  No automatic drop or apply is introduced.
- Treat a terminal fully applied Session as an `already_applied` cleanup no-op
  only after native status and an empty clean apply preview prove no candidates
  or pending apply remain. Batch cleanup treats that outcome as success without
  claiming deletion or rollback. Other native refusals remain failures.
- Present readable native apply plans and scoped detail separately from the
  approval card. Recheck plan consistency during detail collection; do not request
  approval for invalid, blocked or changed previews.
- Keep approval cards concise even when a host collapses whitespace. Show Agent
  intent, project, change count, up to three paths, remaining counts and task ID,
  without raw diff or repeated native banners.

### Caller changes

DSH `pvisor_apply` requires a nonempty `summary` of 1–512 bytes explaining the
reviewed change's intent. It is unverified display text, not test evidence or
write authority. Pi operator commands may omit it. Apply still requires user
confirmation; only drop's additional plugin prompts were removed.

`stdout` for spawn is task output; launcher messages are in `cli`. Consumers
must check `output_status`, task state and exit codes rather than treating an
empty stream or successful transport as task success.

### Acceptance scope

Portable contracts and real Linux CLI cases cover staging, output, Rust offline
builds, fanout, selective apply, conflict refusal and drop. Loader/compiler tests
cover Pi and DSH. The compact card still needs verification in the actual DSH UI.
No Pulsing-wide build, macOS Rust profile or Windows Host stagebox acceptance
is claimed.

## 0.1.1

- Use a separately installed Homebrew/pip runtime instead of native npm packages.
- Remove the same-name npm `pvisor` launcher.
- Read MCP identity/version from package metadata.

## 0.1.0

The published package depends on obsolete npm runtime packages. Upgrade to
0.1.1 or newer for independently installed pVisor runtimes.
