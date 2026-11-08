# Runtime integration-test support

`lease_child.rs` is shared only by the prepared-rootfs and prepared-workspace
integration tests. It owns their self-executed libtest child, exact readiness
line, release byte, bounded waits and cleanup. It is not a public runtime API
or a general subprocess runner.

Keep terse child formatting and exact marker matching together. Do not replace
the lease or garbage-collection assertions with readiness alone. A readiness
failure must preserve observed stdout and kill/reap the child and join its
reader before the fixture can be removed.

The drain reads fixed 1 KiB chunks, keeps only a 4 KiB raw diagnostic tail and
matches the short ASCII marker with constant-size line state. Overlong or
unterminated lines cannot allocate a growing buffer. Noise is never queued:
the reader sends at most one readiness/error notification, and the parent makes
one five-second timed receive. Draining continues after readiness without a
blocking channel send. These bounds cover readiness observation, not arbitrary
descendants or uninterruptible kernel I/O during kill/reap and reader join.
