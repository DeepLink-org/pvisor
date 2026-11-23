# Lazy image command specifications

This feature specifies publication of local Image Pack V1 images through
`pvisor push IMAGE --to URI` (equivalently `pvisor image push`), lazy import
through explicit image pull options, and retirement of `pvisor-daemon image`.
Read [spec.md](spec.md) for
the contract and migration boundaries. Ordinary build/pull output,
image queries and daemon service lifecycle retain their existing meanings.
No new high-level cache command tree is proposed.

These specifications are unreviewed drafts. The cases require PASS, including
both retired daemon spellings. Passing execution is separate from human approval.
The feature follows [issue #161](https://github.com/DeepLink-org/pvisor/issues/161).
Do not edit historical approval records or weaken existing tests to pass them.

## Native Scrut execution

Run from the repository root with matching `pvisor` and `pvisor-daemon`
executables in the same directory:

```sh
mise run cases -- --feature lazy-image --list
mise run cases -- --feature lazy-image --subject-bin "$PWD/target/debug/pvisor"
mise run cases -- --feature lazy-image --subject-bin "$PWD/target/debug/pvisor" --strict
mise run test-spec-fixtures
mise run test-py -- discover -s tests/spec/lazy-image/regression -p 'test_*.py' -v
```

The shared [scenario fixture](../fixture.py) owns fresh HOME, XDG, image,
container and Session directories, bounded process cleanup and retained failure
evidence. Feature resources are in [regression/](regression/README.md); native
Scrut discovers cases without another feature dispatcher or Markdown parser.

Local publication cases require Python 3, Bash, OpenSSL and permission to bind
loopback TCP sockets. They reuse the CLI's synthetic TLS OCI registry and an
owned static HTTP repository. No public registry, AWS account, guest, FUSE,
hypervisor or container service is required. Missing prerequisites are failures,
including in expected-failure cases.
S-LAZY-005 requires an amd64 host: the existing daemon prepare-image fixes its
build platform to linux/amd64 and rejects arm64 hosts even without RUN. Its
platform requirement remains part of the case.
On arm64, run the other implemented cases with
`--case S-LAZY-001,S-LAZY-002,S-LAZY-003,S-LAZY-004,S-LAZY-006,S-LAZY-007,S-LAZY-008,S-LAZY-009 --strict`.
S-LAZY-009 constructs a local prepared fixture from a real native build and
checks the new command workflow on either architecture; it does not replace
S-LAZY-005's prepare-image integration prerequisite.

## Coverage boundaries

The cases exercise real CLI processes, filesystem publication and loopback HTTP
reads. They do not establish S3 authorization, live object-store compatibility,
WAN performance or guest execution. Existing native suites retain those narrower
or broader contracts without duplicated fixture implementations:

```sh
mise run test -- pvisor-cli -- --test cache_backends
mise run test -- pvisor-daemon -- --test image_repository
```

The signed S3 fixture, conditional writes, corruption, retry and failure recovery
remain owned by those suites. Their ignored TLS/hardware cases need their stated
prerequisites and explicit selection; ordinary runs do not execute them.
The KVM guest/mmap/credential test in `cache_backends` is not covered by these
Scrut cases. Preserve its assertions when migrating the frontend.
