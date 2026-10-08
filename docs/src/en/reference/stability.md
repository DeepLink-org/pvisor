# Stability and compatibility

For a long-running pipeline, pin the pVisor version, record format, and agent version. Before upgrading, use representative tasks to verify execution, review, apply, and replay; keep the matching reader for old records.

Current readers check explicit format versions. The following lists interface reading rules and the operations to verify during an upgrade.

## Reading rules available today

| Interface | Current rule |
| --- | --- |
| CLI / Run TOML | Current `--help`/types define behavior; unknown fields reject; check changelog when upgrading |
| Run Bundle | Strict schema 4; unknown versions/old observation contracts reject; no automatic migration promise |
| Event / Journal | Formal version 5; old JSONL incompatible |
| Operation | Schema 1; unknown versions reject |
| Replay | `sandbox-playback.result/v3`; adapter versions pinned in the guide |
| Rust embedding API | Follows crate revision; CLI availability does not imply ABI/source compatibility |

These are implemented reading rules, not new long-term promises awaiting maintainer confirmation. Pin pVisor version/commit, record schemas, and validate representative tasks before upgrading.

## Upgrade procedure

1. Retain old Bundles, Journals, and capture rather than overwriting them.
2. Run a fresh Job with the new version; check admission, output reading, and apply conflicts.
3. Keep a compatible old reader for old records; do not remove schema fields to bypass checks.
4. Record CLI/config/format/platform changes in the changelog. Maintainers still need to define deprecation windows.
