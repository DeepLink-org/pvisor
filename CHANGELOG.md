# Changelog

## Unreleased

### ZCode CLI integration

- Added Run-scoped ZCode proxy and BigModel Gateway profile support.
- Added personal Agent defaults under `XDG_CONFIG_HOME/pvisor/agents/`, explicit
  filesystem mount grants, and Run-local provider catalog snapshots.
- Gateway now decodes gzip and deflate model responses before capture and
  protocol translation.
- Added a deterministic ZCode CLI integration example covering staged writes,
  Gateway SSE capture, apply/drop, persistent state, and timeout cleanup.
- Unified filesystem configuration around `--stage`, `--mount`, and `--access`;
  runs without `--stage` now use a temporary changeset that is dropped
  automatically.

Contributors: [zhaoyuuu0624](https://github.com/zhaoyuuu0624) (original
ZCode CLI integration and Gateway response decoding contribution); pVisor
maintainers (port to the current isolation and file-access policy).
