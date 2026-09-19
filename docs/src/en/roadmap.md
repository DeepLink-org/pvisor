# Roadmap

This roadmap describes product direction, not a promise that every item is
implemented. The [Project](project/index.md) pages and release notes are the
source of truth for delivered behavior.

## Now: make the local loop dependable

- Make pVisor Run → review → apply predictable on macOS and Linux.
- Keep effective controls, Effects, and warnings visible in every Run Bundle.
- Keep English and Chinese documentation paths aligned and examples runnable.

## Next: tighten capture and comparison

- Keep configured capture attached to a stable Run identity.
- Improve comparison of Runs that share a project.
- Document provider-specific boundaries instead of presenting one universal
  isolation claim.

## Later: move from one workstation to a fleet

- Share Dataset catalogs and policies across teams without hiding provenance.
- Support repeatable execution profiles for hosts, OCI containers, and VMs.
- Add operational guidance for retention, access control, and cost-aware
  storage.

## How to read this list

An item is not complete because a design document exists. Look for a working
CLI path, tests or examples, documented limitations, and a release entry before
treating a capability as available. Proposed changes belong in an RFC when
they change a data contract, execution boundary, or public command.
