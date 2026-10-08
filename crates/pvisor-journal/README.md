# pvisor-journal

Single-writer storage and producers for the shared `pvisor-core::event` records.
The public boundary is exclusively `pvisor_journal::api`; import its traits to
call constructors and methods. Core Event, Fact, Record and Receipt contracts
remain owned by Core, not duplicated or re-exported here.

## Architecture

- `src/lib.rs`: public `api` module and private implementation modules only.
- `src/api.rs`: documented declarations, opaque owner exports, `AppendError`,
  and the stateless `Persistence` marker; no method bodies or platform cfgs.
- `src/journal.rs`: storage, recovery, locking, causal validation, receipts and
  live notifications; private-state fault and cancellation tests.
- `src/trace.rs`: private producer identity and event construction.
- `src/persistence.rs`: Unix filesystem durability and private barrier tests.
- `tests/api_contract.rs`: external lifecycle, validation and persistence tests
  plus recursive AST guards for the API boundary.

## Contracts

`JournalStore` constructs volatile or durable journals. Clones share the mutex,
writer descriptor, error state and notification channel. The exclusive file lock
lasts until the last owner (including a Trace or accepted blocking task) drops.
Closed-file read/validate use shared locks and never repair. Open repairs only
an incomplete final record, not a corrupt complete record or incomplete header.
Complete recovered records are synced before a retry can receive LocalSync.

Append validates Core bounds and causality, accepts unresolved forward references,
and rejects cycles. Identical event-ID/content retries return the original
position without new notifications; changed content is rejected. Rejected means
this request did not commit. Unknown means the outcome cannot be inferred;
a write failure poisons all clones until close/reopen recovery. Memory receipts
are Volatile; file receipts are LocalSync, not replicated or remote durability.
Live subscriptions have capacity 256, contain only newly committed events, and
can lag: use records to recover rather than treating the stream as storage.

Async append/emit futures are Send and use Tokio blocking tasks. Once first
polled and accepted by spawn_blocking, cancellation of the waiter does not cancel
the append. Retry the exact same event after recovery if the outcome is unknown.
Snapshot holds the writer mutex, bounds the source size before output, and copies
durable bytes only. Output failures may leave a partial destination; callers own
its cleanup, syncing and publication. They do not poison the journal.

`TraceProducer::with_id` supplies immutable trace identity at construction;
`id`, `producer`, and `journal` are read-only accessors. Construction and event
building do not validate inputs: append does. Emit accepts the provided event
without requiring its trace/producer identity to match the producer.

`DurableFiles` provides Unix directory barriers and atomic replacement. Failures
are not rollback transactions: created directories may remain, temporary cleanup
is best effort, and rename success followed by directory-sync failure leaves the
new destination visible with uncertain crash durability. Observers run after
I/O and cleanup, report attempted steps including failure, and exclude observer
work from durations. There is no portable non-Unix file backend in this crate.

## Usage

```rust
use pvisor_journal::api::{Journal, JournalStore, Trace, TraceProducer};

let journal = Journal::memory();
let trace = Trace::with_id(journal.clone(), "run-identity", "executor");
assert_eq!(trace.id(), "run-identity");
assert!(trace.journal().records()?.is_empty());
# Ok::<(), anyhow::Error>(())
```

## Focused validation

From the repository root:

```sh
just test pvisor-journal
cargo check -p pvisor-journal --all-targets
cargo clippy -p pvisor-journal --all-targets -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc -p pvisor-journal --no-deps
cargo test -p pvisor-journal --doc
```

Consumer migrations belong to their respective crates. No compatibility root
exports or public inherent methods are provided. Filesystem coverage requires a
Unix host with advisory file locking and directory fsync support; passing local
tests does not establish crash/power-loss behavior or other-platform coverage.
