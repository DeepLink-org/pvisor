// SPDX-License-Identifier: MPL-2.0

use aster_fuse::{
    Attr, EntryReply, FUSE_ROOT_ID, FuseGeneration, FuseNodeId, LookupOperation,
    ops::read_index::ReadIndexOperation,
};
use aster_virtio::device::filesystem::device::{AttrVersion, FuseSession};
use hashbrown::HashMap;

use crate::{
    prelude::*,
    thread::work_queue::{self, WorkPriority},
};

const DISTINCT_NAMES: usize = 4;
const FIRST_WINDOW: u32 = 16 * 1024;
const NEXT_WINDOW: u32 = 512 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Scope {
    parent: FuseNodeId,
    epoch: u64,
    credentials: (u32, u32, u32),
}

impl Scope {
    fn current(session: &FuseSession, parent: FuseNodeId) -> Option<Self> {
        let (epoch, credentials) = session.read_index_scope()?;
        Some(Self {
            parent,
            epoch,
            credentials,
        })
    }
}

/// One bounded positive directory image per mount.
pub(super) struct DirIndex {
    session: Arc<FuseSession>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    unsupported: bool,
    demand: Option<Demand>,
    window: Option<Window>,
}

struct Demand {
    scope: Scope,
    names: Vec<String>,
}

struct Window {
    scope: Scope,
    entries: HashMap<String, IndexedEntry>,
    continuation: Option<(u64, usize)>,
    consumed: usize,
}

struct IndexedEntry {
    reply: EntryReply,
    attr_version: AttrVersion,
    // One reference, transferred on the first lookup. Later observations
    // require a matching live dentry and transfer nothing.
    pending: bool,
}

impl DirIndex {
    pub(super) fn new(session: Arc<FuseSession>) -> Self {
        Self {
            session,
            state: Mutex::new(State::default()),
        }
    }

    pub(super) fn observe(
        &self,
        parent: FuseNodeId,
        name: &str,
        nodeid: FuseNodeId,
        generation: FuseGeneration,
    ) -> Option<Attr> {
        let scope = Scope::current(&self.session, parent)?;
        let state = self.state.try_lock()?;
        let window = state.window.as_ref().filter(|w| w.scope == scope)?;
        let entry = window.entries.get(name)?;
        (!entry.pending
            && entry.reply.nodeid() == nodeid
            && entry.reply.generation() == generation
            && Scope::current(&self.session, parent) == Some(scope))
        .then(|| entry.reply.attr())
    }

    pub(super) fn lookup(
        &self,
        parent: FuseNodeId,
        name: &str,
    ) -> Result<(EntryReply, AttrVersion)> {
        let scope = Scope::current(&self.session, parent);
        if let Some(scope) = scope
            && let Some(hit) = self.try_index(scope, name)
        {
            return Ok(hit);
        }

        let attr_version = self.session.snapshot_attr_version();
        let reply = self
            .session
            .do_fuse_op(parent, LookupOperation::new(name))?;
        if reply.nodeid().as_u64() != 0
            && let Some(scope) = scope
            && let Some(mut state) = self.state.try_lock()
            && !state.unsupported
            && Scope::current(&self.session, parent) == Some(scope)
            && state
                .window
                .as_ref()
                .is_none_or(|window| window.scope != scope)
        {
            let demand = state.demand.get_or_insert_with(|| Demand {
                scope,
                names: Vec::new(),
            });
            if demand.scope != scope {
                *demand = Demand {
                    scope,
                    names: Vec::new(),
                };
            }
            if demand.names.len() < DISTINCT_NAMES && !demand.names.iter().any(|n| n == name) {
                demand.names.push(name.to_string());
            }
        }
        Ok((reply, attr_version))
    }

    fn try_index(&self, scope: Scope, name: &str) -> Option<(EntryReply, AttrVersion)> {
        let mut state = self.state.try_lock()?;
        if state.unsupported || Scope::current(&self.session, scope.parent) != Some(scope) {
            return None;
        }

        if state
            .window
            .as_ref()
            .is_some_and(|window| window.scope.epoch != scope.epoch)
        {
            self.discard(state.window.take());
        }
        let (offset, size) =
            if let Some(window) = state.window.as_mut().filter(|w| w.scope == scope) {
                if let Some(entry) = window.entries.get_mut(name) {
                    if !entry.pending {
                        return None;
                    }
                    entry.pending = false;
                    window.consumed += 1;
                    return Some((entry.reply, entry.attr_version));
                }

                let (offset, required) = window.continuation?;
                if window.consumed < required {
                    return None;
                }
                window.continuation = None;
                (offset, NEXT_WINDOW)
            } else {
                let demand = state.demand.as_ref()?;
                if demand.scope != scope
                    || demand.names.len() < DISTINCT_NAMES
                    || demand.names.iter().any(|n| n == name)
                {
                    return None;
                }
                self.discard(state.window.take());
                state.demand = None;
                (0, FIRST_WINDOW)
            };

        let attr_version = self.session.snapshot_attr_version();
        let reply = self
            .session
            .do_fuse_op(scope.parent, ReadIndexOperation::new(offset, size))
            .map_err(Error::from);
        let page = match reply {
            Ok(page) => page,
            Err(err) => {
                state.unsupported = err.error() == Errno::ENOSYS;
                if state.unsupported {
                    self.discard(state.window.take());
                }
                return None;
            }
        };
        let mut entries = HashMap::new();
        let mut forget = Vec::new();
        let mut valid = page.valid
            && if page.complete {
                page.next_offset >= offset
            } else {
                page.next_offset > offset
            };
        for record in page.records {
            let entry = record.entry;
            if entry.nodeid().as_u64() == 0
                || entry.nodeid() == FUSE_ROOT_ID
                || (entry.entry_valid() == 0 && entry.entry_valid_nsec() == 0)
            {
                continue;
            }
            if let Some(name) = record.name {
                // The image's lifetime is not a general inode or dentry TTL.
                let reply =
                    EntryReply::new(entry.nodeid(), entry.generation(), 0, 0, 0, 0, entry.attr());
                let entry = IndexedEntry {
                    reply,
                    attr_version,
                    pending: true,
                };
                if state
                    .window
                    .as_ref()
                    .is_some_and(|w| w.entries.contains_key(&name))
                {
                    valid = false;
                }
                if let Some(previous) = entries.insert(name, entry) {
                    valid = false;
                    forget.push((previous.reply.nodeid(), 1));
                }
            } else {
                forget.push((entry.nodeid(), 1));
            }
        }
        if !valid || Scope::current(&self.session, scope.parent) != Some(scope) {
            forget.extend(entries.into_values().map(|entry| (entry.reply.nodeid(), 1)));
            self.forget(forget);
            return None;
        }
        self.forget(forget);

        let hit = entries.get_mut(name).map(|entry| {
            entry.pending = false;
            (entry.reply, entry.attr_version)
        });
        let window = state.window.get_or_insert_with(|| Window {
            scope,
            entries: HashMap::new(),
            continuation: None,
            consumed: 0,
        });
        if offset == 0 && hit.is_some() && !page.complete {
            window.continuation = Some((page.next_offset, entries.len() - entries.len() / 4));
        }
        window.consumed += usize::from(hit.is_some());
        window.entries.extend(entries);
        hit
    }

    fn discard(&self, window: Option<Window>) {
        if let Some(window) = window {
            self.forget(
                window
                    .entries
                    .into_values()
                    .filter(|entry| entry.pending)
                    .map(|entry| (entry.reply.nodeid(), 1))
                    .collect(),
            );
        }
    }

    fn forget(&self, entries: Vec<(FuseNodeId, u64)>) {
        if entries.is_empty() {
            return;
        }
        let session = self.session.clone();
        work_queue::submit_work_func(
            move || {
                if let Err(err) = session.forget_many(&entries) {
                    warn!("virtiofs directory index forget failed: {:?}", err);
                }
            },
            WorkPriority::Normal,
        );
    }
}

impl Drop for DirIndex {
    fn drop(&mut self) {
        let window = self.state.get_mut().window.take();
        self.discard(window);
    }
}
