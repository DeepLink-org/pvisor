---
status: todo
search:
  exclude: true
---

# Stability and compatibility

!!! warning "Planned"
    Maintainers must decide the version policy.

## Question

Which interfaces can users rely on to stay unchanged? What compatibility rules apply to CLI options, configuration fields, the Run Bundle schema, JSON output, and the embedding API?

## Requirements

- Maintainer-defined version policy, including pre-1.0 incompatible changes.
- Stability and deprecation notice period per interface.
- Old-record handling on Bundle upgrades.

## Acceptance criteria

- Maintainer confirmation.
- Changelog marks incompatible changes accordingly.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: Maintainers
- Related: [Run Bundle](run-bundle.md), [changelog](../community/changelog.md)

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
