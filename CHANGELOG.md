# Changelog

## Unreleased

### Unified Session lifecycle terminology

- Use `Session` for durable work, `SessionService` for lifecycle operations and
  `ExecutionContext` for the current executor context. Execution identities use
  `execution_id`; CLI selectors and control targets use Session terminology.
- This is a source and wire compatibility break: Host AgentCtl is version 2,
  instance control is version 3, modern Run records are schema 3, Run Bundles are
  schema 6, completion receipts are version 2 and Session continuation records
  are version 3. Status output is schema 3. Keep the matching old binary for
  existing records; no records or snapshot stores are automatically rewritten.
- Build-tool parallelism options and imported third-party formats retain their
  upstream names. Existing historical evidence and human approval ledgers remain
  unchanged; updated semantic specifications require human review again.

### Landlock-only filesystem isolation

- Add a filesystem policy that installs Landlock against the host view without
  creating user, mount or PID namespaces, so `--filesystem sandbox` keeps
  enforcing a path boundary on hosts that deny `mount(2)` instead of silently
  returning to an unisolated host process.
- Withhold `/` and the projected home and XDG roots in that mode. Without a
  synthetic root to project them behind, either grant would expose every sibling
  of the workspace; the launcher redirects `HOME` into a private temporary
  directory instead.
- Record the installed mechanism as `linux-landlock-host` and name the boundary
  actually in effect, rather than always reporting namespace isolation.
- Keep the projected host procfs when the runtime denies mounting a fresh one,
  instead of failing the whole sandbox setup.

### Rootless capability probe

- Replay the launcher's own setup in a child process instead of approximating it
  with `unshare(1)`. The approximation was wrong in both directions: the `mount`
  binary refuses a non-root euid under the launcher's identity mapping (false
  negative on hosts where the sandbox works), and the probe never exercised the
  `/proc` projection the synthetic root starts with, so a privileged container
  that creates every namespace but cannot project `/proc` inside them was
  reported as capable and then aborted during Agent setup (false positive).
- Map the caller's own uid in the probe, matching the `uid_map`/`gid_map` writes
  the launcher always performs, so a kernel that allows `CLONE_NEWUSER` but
  rejects that write no longer reports the host as capable.

### Snapshot RAM mount isolation

- Keep transient snapshot RAM mounts and pager specifications in validated private
  host runtime directories, separate from snapshot stores and node state. The SDK
  `directory` argument is retained but now identifies an excluded state root,
  rather than the mount parent.
- Bound RAM cleanup-helper waits and clean up owned mounts after abnormal helper
  exits; persistent snapshot data and unrelated legacy mounts remain untouched.
- Reject unsupported nested mounts during Linux rootless state staging with
  topology diagnostics and the original mount error, without weakening isolation.

### ZCode CLI integration

- Added Run-scoped ZCode proxy and BigModel Gateway profile support.
- Added personal Agent defaults under `XDG_CONFIG_HOME/pvisor/agents/`, explicit
  filesystem mount grants, and Run-local provider catalog snapshots.
- Gateway now decodes gzip and deflate model responses before capture and
  protocol translation.
- Added a deterministic ZCode CLI integration example covering staged writes,
  Gateway SSE capture, apply/drop, persistent state, and timeout cleanup.
- Unified filesystem configuration around `--stage`, `--mount`, and `--access`.
  Ordinary host Sessions write through; `--safe` retains workspace
  changes in Session storage by default. `--stage` selects an explicit stage.

Contributors: [zhaoyuuu0624](https://github.com/zhaoyuuu0624) (original
ZCode CLI integration and Gateway response decoding contribution); pVisor
maintainers (port to the current isolation and file-access policy).
