//! Journal storage, trace production and Unix durable-file contracts.
//!
//! Import the corresponding trait to use an opaque owner's methods. Shared
//! records are owned by `pvisor_core::event`. Cloning owners shares journal
//! storage, writer locks and notifications, but copies immutable trace identity.
//! File operations require Unix advisory locks and directory fsync support;
//! unsupported filesystem operations return I/O errors, not weaker durability.
//!
//! ```
//! use pvisor_journal::api::{Journal, JournalStore, Trace, TraceProducer};
//!
//! let journal = Journal::memory();
//! let trace = Trace::with_id(journal.clone(), "run-identity", "executor");
//! assert_eq!(trace.id(), "run-identity");
//! assert!(trace.journal().records()?.is_empty());
//! # Ok::<(), anyhow::Error>(())
//! ```

use pvisor_core::event::{Event, Fact, Receipt, Record};
use std::{future::Future, io::Write, path::Path, time::Duration};

pub use crate::journal::Journal;
pub use crate::trace::Trace;

/// An append failed either before committing or with an uncertain outcome.
#[derive(Debug, thiserror::Error)]
pub enum AppendError {
    /// This request did not commit: invalid Core event, serialization/bounds
    /// failure, changed content for an existing ID, or a causal cycle. Previously
    /// committed records remain intact; the handle is not poisoned by rejection.
    #[error("trace append rejected: {0}")]
    Rejected(String),
    /// Commit cannot be established (mutex, blocking-task or write/sync failure).
    /// A file write failure poisons every clone: records/snapshot and subsequent
    /// valid appends fail until all owners drop and the file is reopened. Bytes
    /// may already contain a complete or partial record. Recover and retry the
    /// exact same event ID/content; never assume that this error means absence.
    #[error("trace append outcome unknown; reopen journal before retry: {0}")]
    Unknown(String),
}

/// Single-writer storage operations. Journal clones serialize through one mutex;
/// their exclusive durable descriptor survives until the final clone, producer,
/// or accepted blocking task drops. Default constructs an independent memory
/// journal. No operation replicates data or guarantees remote durability.
pub trait JournalStore {
    /// Create independent empty volatile storage without I/O, with a random
    /// journal ID. Receipts are Volatile and snapshots are unsupported.
    fn memory() -> Self
    where
        Self: Sized;

    /// Open/create a mode-0600 file without following a final-path symlink and
    /// acquire a nonblocking exclusive writer lock. Creates/syncs parent entries;
    /// even failure may leave directories/files or changed permissions. An empty
    /// file receives a synced header. Existing complete records are validated
    /// (Core bounds, contiguous positions, unique IDs, acyclic causal graph) and
    /// synced before use. Only a non-newline-terminated final record is truncated
    /// and synced; oversized tails, incomplete headers and corrupt complete
    /// records fail rather than silently repair. Unresolved causes are allowed.
    fn open(path: &Path) -> anyhow::Result<Self>
    where
        Self: Sized;

    /// Read a closed file under a nonblocking shared lock. Validate header and
    /// records as open does, but never repair or modify bytes/permissions. A live
    /// writer, incomplete tail, unsupported format or corruption returns error.
    /// Retains all records and identity/causality indexes in memory.
    fn read(path: &Path) -> anyhow::Result<Vec<Record>>
    where
        Self: Sized;

    /// Validate a closed file with the same locking/no-repair rules as read,
    /// without retaining payloads. Identity and causality indexes still grow with
    /// record count. Lines exceeding Core's maximum event bytes plus 4096 bytes
    /// of record overhead fail; this is not an unbounded JSON line reader.
    fn validate(path: &Path) -> anyhow::Result<()>
    where
        Self: Sized;

    /// Subscribe to future newly committed events, not history. Sends occur after
    /// storage commit (and file sync) while append is serialized. Identical retry
    /// receipts and rejected/failed writes are quiet. A blocking-task join error
    /// can still hide a completed append and its notification. Capacity is 256 events;
    /// broadcast lag is reported by the receiver, recoverable through records.
    /// Dropping the final sender closes receivers after buffered events drain.
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Event>;

    /// Consume and validate an event using Core bounds and semantic checks:
    /// current version; nonblank ID/trace/producer at most 256 bytes each; 1..=16
    /// nonblank scope entries at most 256 bytes each; at most 64 distinct nonempty
    /// causal IDs, each at most 256 bytes and not self; optional context/operation
    /// IDs nonempty and at most 256 bytes; valid fact-specific contracts; serialized
    /// event at most MAX_EVENT_BYTES (currently 1 MiB). Positions must fit u64.
    /// Serialize writes, assign zero-based contiguous positions, allow unresolved
    /// forward causes, and reject cycles. Same ID and serialized content returns
    /// its prior position without rewriting or notification; different content
    /// is Rejected. File receipts are LocalSync only after file sync, including
    /// recovered duplicates; memory receipts are Volatile. Rejected is definitely
    /// uncommitted by this call; Unknown is not (see AppendError). Indexes retain
    /// IDs/digests/causes for the handle's lifetime; memory also retains payloads.
    fn append(&self, event: Event) -> Result<Receipt, AppendError>;

    /// Execute append on Tokio's blocking pool; requires a running Tokio runtime.
    /// The Send future borrows this handle until dropped/completed. Before first
    /// poll no work is submitted; after submission the task owns a clone and
    /// outlives cancellation of the waiter. A join failure is Unknown; an exact
    /// retry resolves idempotently rather than assuming cancellation prevented
    /// commit. Blocking work cannot be aborted by dropping this future.
    fn append_async(
        &self,
        event: Event,
    ) -> impl Future<Output = Result<Receipt, AppendError>> + Send;

    /// Return owned records while excluding appends. Durable files are rescanned
    /// and validated without repair; memory records are cloned. Poisoned handles
    /// fail rather than exposing potentially uncertain state. Mutating the
    /// returned vector cannot modify storage. Retains payloads for all records.
    fn records(&self) -> anyhow::Result<Vec<Record>>;

    /// Copy durable source bytes under the append mutex, excluding later appends.
    /// Reject volatile/poisoned handles and source size above max_bytes before
    /// writing any output. Sync source, then copy at most the captured size;
    /// does not repair or separately rescan source. Caller owns output cleanup,
    /// sync and publication. I/O errors can leave partial output but do not poison
    /// this handle; subsequent appends seek to EOF independently of this cursor.
    fn snapshot_to(&self, output: &mut impl Write, max_bytes: u64) -> anyhow::Result<u64>;
}

/// Immutable producer identity paired with a shared journal. Construction and
/// event building do not validate identity or other inputs; append validates.
/// Clones retain identity and share storage, never create another writer.
pub trait TraceProducer {
    /// Consume a journal handle and producer string; generate a random UUID trace
    /// identity without I/O. Empty/oversized producer is rejected only on append.
    fn new(journal: Journal, producer: impl Into<String>) -> Self
    where
        Self: Sized;

    /// Consume inputs and retain the supplied trace identity exactly, without
    /// validation, mutation setters, I/O, or allocating a replacement journal.
    /// Empty/oversized identities fail Core validation when an event is appended.
    fn with_id(journal: Journal, id: impl Into<String>, producer: impl Into<String>) -> Self
    where
        Self: Sized;

    /// Borrow the immutable trace identity for this producer's lifetime.
    fn id(&self) -> &str;
    /// Borrow the immutable producer name for this producer's lifetime.
    fn producer(&self) -> &str;
    /// Borrow an opaque shared journal handle, without transferring ownership.
    /// Cloning it extends the writer-lock lifetime; storage remains mutable only
    /// through JournalStore, not by accessing implementation state.
    fn journal(&self) -> &Journal;

    /// Build an unvalidated owned Core event with a fresh UUID, current Unix-ms
    /// timestamp and this producer's identity; retain supplied fields exactly.
    /// Context facts use Detail, others Operation. Completed denied/unsupported
    /// failures use Warn, other failures Error, all other facts Info. Construction
    /// neither appends nor reserves a journal position or causal identity.
    fn event(
        &self,
        scope: Vec<String>,
        context: Option<String>,
        operation: Option<String>,
        caused_by: Vec<String>,
        data: Fact,
    ) -> Event;

    /// Append the supplied event asynchronously and return its receipt's event ID;
    /// does not overwrite or require matching trace/producer identity. Validation,
    /// durability, cancellation and runtime rules are those of append_async.
    /// AppendError is preserved as an anyhow error for downcasting.
    fn emit(&self, event: Event) -> impl Future<Output = anyhow::Result<String>> + Send;
}

/// Stateless marker for Unix durable-file operations; import DurableFiles.
pub struct Persistence;

/// Synchronous Unix filesystem barriers. Errors propagate without claiming
/// rollback, unsupported barriers never silently downgrade to volatile writes.
pub trait DurableFiles {
    /// Open path and sync_all its descriptor. Intended for directory barriers;
    /// does not create paths or validate that path is a directory. Missing paths,
    /// permissions and unsupported filesystem sync operations return I/O errors.
    fn sync_directory(path: &Path) -> anyhow::Result<()>;

    /// Create the entire tree then sync the first existing parent and each new
    /// directory once, ancestor-first, committing child entries. Existing trees
    /// perform no barriers. On creation/sync failure some/all new directories
    /// remain: the caller owns cleanup and recovery of partial durability.
    fn create_dir_all_durable(path: &Path) -> anyhow::Result<()>;

    /// Atomically replace path using a unique create-new sibling temporary file,
    /// Unix permissions mode, write_all, file sync, rename and parent sync; first
    /// durably create the parent tree. No rollback: rename success followed by
    /// sync failure leaves the replacement visible with uncertain crash durability.
    /// Before rename failure the destination is unchanged (parent entries may
    /// exist); temporary removal on error is best effort. Does not preserve the
    /// old file's mode or follow a final destination symlink as a write target.
    fn atomic_write(path: &Path, contents: &[u8], mode: u32) -> anyhow::Result<()>;

    /// Same replacement semantics as atomic_write, reporting attempted phases
    /// after all I/O and error cleanup, in order: directory_prepare,
    /// directory_prepare_sync, file_write (create/mode/write), file_sync, rename,
    /// directory_sync. A failure stops later phases; parent preparation reports
    /// both preparation phases with the same success flag. Paths without a parent
    /// fail before any report. Durations exclude observer work; preparation time
    /// excludes its separately accumulated sync duration. Original errors survive
    /// observation unless the callback panics, which propagates after side effects.
    /// Observer is synchronous, need not be Send, and cannot veto or undo a write.
    fn atomic_write_observed(
        path: &Path,
        contents: &[u8],
        mode: u32,
        observe: impl FnMut(&'static str, Duration, bool),
    ) -> anyhow::Result<()>;
}
