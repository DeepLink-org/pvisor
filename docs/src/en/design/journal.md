# Journal design

## 1. Motivation {#motivation}

A Run produces admission, rewriting, placement, dispatch, results and Gateway observations. They can come from different asynchronous tasks, with timestamps that are not strictly monotonic. Developers need recoverable facts answering which events committed, in what order, and whether retries duplicated them, without relying on stderr or live delivery.

Journal chooses one writer, JSONL and per-record synchronization. Each Receipt contains event ID, Journal position and durability. Producers can publish committed facts after receiving it, and lagging consumers can inspect history. The cost is one sync per new disk event and full scans for recovery/history.

It stores execution facts without atomically joining append to external requests. LocalSync is not cross-node replication or external-effect exactly-once, nor a common commit point for RunRecord, Bundle and file apply ledgers. Event fields belong in [Operation and Event](operations-events.md); this page covers storage, commit and recovery.

## 2. Core design {#core-design}

### One state and one commit order {#ownership}

The public boundary is exclusively `pvisor_journal::api`: import `JournalStore`, `TraceProducer` and `DurableFiles` for storage, producer and durable-file operations. `Journal` and `Trace` are opaque owners with no public mutable fields; `Trace::with_id` sets explicit, immutable trace identity at construction. Core owns the shared Event, Fact, Record and Receipt contracts. Implementation stays in private `journal.rs`, `trace.rs` and `persistence.rs` modules.

Journal clones share `Arc<Mutex<State>>`. State owns the journal ID, optional file FD, deduplication index, causal graph, unresolved-reference set and poisoned flag. Disk mode holds an exclusive file lock; a process-local mutex serializes appends to the same Journal. Memory mode stores `Vec<Record>` and returns Volatile receipts.

| Data | Content/purpose |
|---|---|
| `seen` | Event ID → `(offset, SHA-256(serialized Event))`, locating retries and checking content equality |
| `causes` | Event ID → caused_by list, checking known-graph cycles |
| `unresolved` | Causal IDs not yet present, avoiding graph traversal on ordinary appends |
| `memory` | Full Records only in memory mode; disk mode rereads the file |
| `file` / `poisoned` | FD and isolation after unknown write outcomes |
| `live` | Tokio broadcast with capacity 256, delivering post-commit Events |

Position offset is a zero-based record ordinal, not a byte offset or observation clock. It orders commits within one Journal. `caused_by` supplies causal edges, allowing temporarily unresolved forward references. Neither replaces external request ordering or transaction sequencing.

### Disk file and in-memory indexes {#disk-layout}

![Journal JSONL byte layout, indexes and file relationships](../../zh/design/assets/journal-layout.svg)

Figure annotations are in Chinese.

```text
<recording destination>/
└── events.trace.jsonl         One header line plus zero or more Record lines; writer mode 0600
```

CLI `JournalRecording::open()` uses that filename when destination has no extension; otherwise destination itself is the file. Lock ownership resides on this FD, without a separate `.lock` or WAL. New parent directories use durable creation and directory-entry syncing, without guaranteeing private 0700 permissions for the entire path. Callers should select trusted directories.

The file is UTF-8 JSON text without fixed binary pages, length prefixes or persisted hash footers. Indexes live in memory and are rebuilt by scanning. There is no sidecar index, segmentation or automatic GC. SHA-256 supports deduplication in the current handle rather than an on-disk tamper-evident chain.

## 3. Detailed data and mechanisms {#detailed-design}

### JSONL layout and schema {#format}

Current `pvisor-core::event::VERSION` is 5. The first line is Header; each later line is a Record. Every complete line ends in LF (`0x0a`). This structurally valid example uses illustrative identities/time:

```jsonl
{"format":"pvisor.trace/5","journal":"demo-journal"}
{"position":{"journal":"demo-journal","offset":0},"event":{"version":5,"id":"event-0","trace_id":"trace-demo","producer":"demo","observed_at_unix_ms":0,"scope":["runtime:demo"],"context":null,"operation":null,"caused_by":[],"level":"info","granularity":"operation","data":{"fact":"observation","domain":"runtime","name":"example","version":1,"payload":null}}}
```

| Structure | Fields/checks |
|---|---|
| Header | `format` must be `pvisor.trace/5`; `journal` nonempty and at most 256 B; unknown fields rejected |
| Record | `position: {journal, offset}` and complete Event; journal matches Header and offsets are consecutive |
| Event | Version, ID, trace_id, producer, observed_at_unix_ms, scope, context, operation, caused_by, level, granularity, data |
| Receipt | `event`, `position`, `durability`; not separately stored |

Fact uses a `fact`-tagged enum. Header, Record, Position, Event and Fact reject unknown fields. Core validates identity lengths, scope size/content, at most 64 caused_by references, self/duplicate-reference rejection, fact-specific references and field combinations. Serialized Event size is at most 1 MiB.

With Header JSON length H and Record JSON lengths Rᵢ, logical file length is `H + 1 + Σ(Rᵢ + 1)`, and record i starts at `H + 1 + Σⱼ<i(Rⱼ + 1)`. Filesystem allocation is separate. Scanner lines are limited to `MAX_EVENT_BYTES + 4096`; oversized lines fail, so an arbitrarily large EOF tail is not automatically repairable.

### Open, locking and initialization {#open}

`Journal::open()` creates parents and opens the log read/write/create with mode 0600 and `O_NOFOLLOW`, reapplies permissions and attempts an exclusive lock. `O_NOFOLLOW` constrains the final path component rather than authenticating the whole directory chain. A second cooperating writer cannot simultaneously open the locked file; bypassing programs remain outside the lock protocol.

Empty files receive Header/LF followed by file and parent-directory sync. Nonempty files undergo tail-repair scan and another `sync_all`, ensuring a recovered complete record is synced before a retry can receive LocalSync. It then rebuilds seen, causes and unresolved.

`Journal::read()` takes a shared lock, requiring writer release, and never repairs/truncates. `records()` uses the shared writer state: disk mode scans under the mutex, memory mode clones its Vec. All Journal clones and accepted blocking writes retain the state, so dropping one outer handle may not release the writer lock.

### Append and receipts {#append}

`append()` proceeds through:

1. Validate Event, compute its serialized digest and acquire the mutex.
2. Reject poisoned state; compare a previously seen ID's digest, returning the original Receipt for identical content without another append or notification.
3. Check whether the new node closes a causal cycle, allocate `seen.len()` as offset, serialize Record and add LF.
4. Disk mode seeks EOF, write_all and sync_all; memory mode pushes Record into Vec.
5. Update seen/unresolved/causes, send a live Event and return Receipt.

Normal disk receipts follow synchronization and index updates. Deduplication compares Event reserialization, not original JSON whitespace. Altering timestamp, producer or payload under the same ID is rejected. `Trace::event()` generates a new UUID each call; preserve the original Event for retries instead of calling the factory again.

`Trace` owns trace ID/producer and constructs facts, default levels/granularity and timestamps. `emit()` returns only receipt.event. Call append directly to retain position and durability. The separate `atomic_write()` helper serves Overlay metadata with temporary-file/rename replacement rather than Event append.

### Forward causal references {#causality}

Event validation forbids self-reference. A node not previously referenced cannot close an existing cycle, so append walks known edges only when its ID occurs in unresolved. Finding itself yields Rejected; unknown parents remain unresolved.

Full scans use iterative DFS to avoid recursive-stack overflow on long chains. Validation covers known nodes without requiring all forward references to resolve at EOF or proving operation/context definitions occur in this Journal. Timestamps do not participate in cycle checking or position allocation.

### Unknown outcomes, poisoning and tail repair {#recovery}

| Condition | Outcome/behavior |
|---|---|
| Invalid Event, conflicting ID or causal cycle | Rejected, without accepting this new record |
| Seek/write/sync failure | Unknown and poisoned; partial or complete bytes may exist |
| Append/records after poisoning | Reject continued use; release all clones and reopen |
| Complete valid record present but receipt lost | Reopen syncs/rebuilds indexes; identical-ID retry returns the original position |
| Final Record lacks LF | Open can truncate/sync to the last complete line; read/records fail |
| Complete LF line with invalid JSON, broken positions, duplicate IDs, unsupported version or causal cycle | Fail without skipping/deleting complete lines |
| Incomplete/invalid Header | Open fails rather than treating it as a recoverable record tail |

Scan verifies the complete prefix and its causal graph before truncating. Even complete JSON without LF is an unfinished Record. After truncation that Event is absent and may be appended with the original ID. A complete line may already be present after Unknown, so Unknown does not mean uncommitted.

Validation is not tamper evidence. Replacing payload with another valid JSON value while preserving identity/graph constraints is not detected by a persisted digest chain. Directory/file permissions define the current trust boundary. Preserve corrupt files rather than deleting complete lines to regain readability.

### Async cancellation and live subscriptions {#async-live}

`append_async()` submits a Journal clone and Event to `spawn_blocking`. Once submitted, cancellation of its waiter does not cancel the underlying append; it can acquire the lock later and commit. Blocking-task failure also returns Unknown. After timeout, inspect the stable Event ID instead of redispatching an external action to fill a perceived logging gap.

Live send follows successful commit; duplicate receipts do not resend and absent receivers do not affect append. Broadcast retains at most 256 Events by count rather than bytes; slow readers receive Lagged. Notifications lack persistent positions; event IDs can correlate them to Records. Subscription is not complete replay, a durable cursor or a consumer-acknowledgment protocol.

Outside the broadcast buffer, disk-mode seen/causes grow with events/edges. Open and each records call read full history, without paging or a total-Journal quota. Introduce segmentation/indexing only when measurements justify it while preserving identity/recovery contracts.

## 4. Experimental evidence {#experiments}

This revision reads source/existing documentation without compiling or running product tests. Source supplies two Journal unit tests and four pvisor integration tests. The numbers below describe test construction/assertions rather than fresh measurements.

| Source/test | Scenario and assertions |
|---|---|
| `pvisor-journal/src/journal.rs::write_error_requires_recovery_before_another_receipt` | Read-only FD injection; Unknown blocks subsequent append/records; reopen yields offset 0 and LocalSync |
| `cancelling_waiter_does_not_cancel_accepted_append` | Mutex-blocked accepted append survives dropped waiter Future; repeated submission retains one record |
| `pvisor/tests/trace_journal.rs::durable_identity_and_idempotence_survive_reopen_and_truncated_tail` | Writer lock, identical-ID retry, conflicting content, read-only nonrepair, reopen truncation and position continuity |
| `complete_corruption_and_old_formats_are_never_silently_repaired` | Complete corrupt lines/old versions rejected with bytes preserved |
| `forward_causal_references_resolve_but_cycles_are_rejected` | Forward references resolve; cycles/self-reference rejected |
| `concurrent_appends_have_unique_positions_and_duplicate_retries_converge` | 40 tasks: 20 share one Event and 20 are independent; asserts 21 consecutive records |

These cover deduplication, order and some failure boundaries. Read-only FD injection does not simulate every post-write sync_all failure. Cancellation uses memory mode rather than disk durability, and concurrent-position checks are not throughput benchmarks. A full process/power-loss matrix, long-log recovery, subscriber-lag recovery and storage-device failures remain unmeasured here.

No disk throughput, p99 fsync or recovery peak-memory results are attributable to this documentation revision. Source confirms one sync_all per new disk Event, no file rewrite on duplicate submission, and full open/records scans. Growth and synchronization cost should guide the next measurements.

## 5. Usage recommendations {#usage}

Use `Journal::open()` with a trusted directory when disk receipts are required. Default/memory Journals provide only Volatile. Share clones across producers instead of reopening the same file for competing writers. CLI `finish()` performs no extra batch flush; successful append already synced each record.

Retain a stable ID and complete Event until commit, then keep its Receipt. After Unknown or waiter cancellation, stop reusing an untrusted handle, release clones/active writers and recover/check records. Do not generate a fresh ID to conceal uncertainty or redo remote effects as a log retry.

Read complete history from Journal and use live only for timely display. Handle Lagged explicitly. Query history through the existing writer's records method; independent read requires writer closure. Large Events can make even a 256-entry broadcast costly, so measure payload sizes and consumer speed together.

Preserve damaged logs alongside RunRecord, Bundle and apply ledger for joint diagnosis. They have no common atomic commit point, so one missing artifact does not establish that the Run never executed. Measure append sync, index growth, scans and recovery separately. Group commit/distributed logs are not assumed here: define receipt strength, recovery and compatible formats before changing implementation.
