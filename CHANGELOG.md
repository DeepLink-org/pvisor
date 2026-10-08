# Changelog

## Unreleased

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
  Ordinary host Jobs write through; `--safe` and `--ask` retain workspace
  changes in Job storage by default. `--stage` selects an explicit stage.

Contributors: [zhaoyuuu0624](https://github.com/zhaoyuuu0624) (original
ZCode CLI integration and Gateway response decoding contribution); pVisor
maintainers (port to the current isolation and file-access policy).
