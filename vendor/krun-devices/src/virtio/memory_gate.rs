//! VM-local barrier for device RAM access, including retained descriptor slices.
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};
use vm_memory::{GuestMemory, GuestMemoryMmap};

#[derive(Default)]
struct State {
    closed: bool,
    active: usize,
    prepare: Option<Arc<MemoryPrepare>>,
}
/// Prepare RAM before any queue access, including descriptor-table reads.
/// A preparation error terminates the isolated VMM process: queue APIs cannot
/// safely continue with absent RAM or report a recoverable memory fault.
/// Empty ranges mean conservative whole-RAM preparation for unaudited callers.
pub type MemoryPrepare = dyn Fn(&[(u64, usize)]) -> Result<(), String> + Send + Sync;
#[derive(Default)]
pub struct MemoryGate {
    state: Mutex<State>,
    changed: Condvar,
}
pub struct Access {
    gate: Arc<MemoryGate>,
}

impl MemoryGate {
    /// Optional maintenance must skip retained device buffers without waiting.
    /// A busy result leaves the gate unchanged; success blocks new accesses.
    pub fn try_close(&self) -> Result<bool, &'static str> {
        let mut state = self.state.lock().map_err(|_| "device memory gate poisoned")?;
        if state.active != 0 {
            return Ok(false);
        }
        state.closed = true;
        Ok(true)
    }
    /// Install only after CPU pause and device drain. No callback runs under
    /// the gate lock; its access lease prevents concurrent mapping replacement.
    pub fn set_prepare(&self, prepare: Option<Arc<MemoryPrepare>>) -> Result<(), &'static str> {
        let mut state = self.state.lock().map_err(|_| "device memory gate poisoned")?;
        if !state.closed || state.active != 0 {
            return Err("device RAM preparation requires a drained gate");
        }
        state.prepare = prepare;
        Ok(())
    }
    /// Close at the first point with no outstanding device memory references.
    /// While draining, nested accesses must remain possible. Once idle, all new
    /// access blocks until resume. A busy device causes timeout, not unsafe offload.
    pub fn close(&self, timeout: Duration) -> Result<(), &'static str> {
        let deadline = Instant::now() + timeout;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "device memory gate poisoned")?;
        state.closed = true;
        while state.active != 0 {
            let (next, timed) = self
                .changed
                .wait_timeout(state, deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| "device memory gate poisoned")?;
            state = next;
            if timed.timed_out() && state.active != 0 {
                return Err("device RAM access did not drain");
            }
        }
        Ok(())
    }
    pub fn open(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = false;
        self.changed.notify_all();
    }
    pub fn is_idle_closed(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.closed && state.active == 0
    }
    pub fn has_prepare(&self) -> bool {
        self.state.lock().unwrap().prepare.is_some()
    }
    #[cfg(test)]
    fn enter(self: &Arc<Self>) -> Arc<Access> {
        self.enter_ranges(&[])
    }
    fn enter_ranges(self: &Arc<Self>, ranges: &[(u64, usize)]) -> Arc<Access> {
        let mut state = self.state.lock().unwrap();
        while state.closed && state.active == 0 {
            state = self.changed.wait(state).unwrap();
        }
        state.active += 1;
        drop(state);
        let access = Arc::new(Access { gate: self.clone() });
        access.prepare(ranges);
        access
    }
}
impl Access {
    /// The existing lease prevents mappings changing between descriptor decode
    /// and payload preparation. The callback runs outside the gate lock.
    pub(crate) fn prepare(&self, ranges: &[(u64, usize)]) {
        let prepare = self.gate.state.lock().unwrap().prepare.clone();
        if let Some(prepare) = prepare {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| prepare(ranges)));
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    error!("device RAM preparation failed: {error}");
                    std::process::abort();
                }
                Err(_) => {
                    error!("device RAM preparation panicked");
                    std::process::abort();
                }
            }
        }
    }
}
impl Drop for Access {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock().unwrap();
        state.active -= 1;
        if state.active == 0 {
            self.gate.changed.notify_all();
        }
    }
}

type Registry = Mutex<BTreeMap<usize, Weak<MemoryGate>>>;
fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}
fn key(mem: &GuestMemoryMmap) -> usize {
    mem.iter().next().expect("device memory is empty").as_ptr() as usize
}
/// Register before device activation. Weak entries do not retain terminated VMs.
pub fn register(mem: &GuestMemoryMmap) -> Arc<MemoryGate> {
    let gate = Arc::new(MemoryGate::default());
    let mut registry = registry().lock().unwrap();
    registry.retain(|_, gate| gate.strong_count() != 0);
    registry.insert(key(mem), Arc::downgrade(&gate));
    gate
}
// ponytail: lookup per queue operation; cache the gate in queues if profiling
// shows registry contention. Unregistered upstream/test memory needs no barrier.
#[cfg(test)]
pub(crate) fn access(mem: &GuestMemoryMmap) -> Option<Arc<Access>> {
    access_ranges(mem, &[])
}
pub(crate) fn access_ranges(mem: &GuestMemoryMmap, ranges: &[(u64, usize)]) -> Option<Arc<Access>> {
    let gate = registry()
        .lock()
        .unwrap()
        .get(&key(mem))
        .and_then(Weak::upgrade);
    gate.map(|gate| gate.enter_ranges(ranges))
}

#[cfg(test)]
mod tests {
    #[test]
    fn optional_close_skips_live_views_and_does_not_close_the_gate() {
        let gate = Arc::new(MemoryGate::default());
        let held = gate.enter();
        assert!(!gate.try_close().unwrap());
        assert!(!gate.state.lock().unwrap().closed);
        let sibling = gate.enter();
        drop(held);
        assert!(!gate.try_close().unwrap());
        drop(sibling);
        assert!(gate.try_close().unwrap());
        assert!(gate.is_idle_closed());
        gate.open();
        drop(gate.enter());
    }
    use super::*;
    #[test]
    fn descriptor_header_and_payload_are_prepared_before_reads() {
        use std::io::Read;
        use vm_memory::{Bytes, GuestAddress};
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let memory = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), page * 2)]).unwrap();
        let host = memory.iter().next().unwrap().as_ptr() as usize;
        let gate = register(&memory);
        let restored = memory.clone();
        let ranges_seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = ranges_seen.clone();
        gate.close(Duration::ZERO).unwrap();
        // A descriptor read before preparation must fault, rather than merely
        // observing zero-filled memory. Payload occupies a separate host page.
        assert_eq!(unsafe { libc::mprotect(host as *mut _, page * 2, libc::PROT_NONE) }, 0);
        gate.set_prepare(Some(Arc::new(move |ranges| {
            recorded.lock().unwrap().push(ranges.to_vec());
            if ranges == [(0, 16)] {
                assert_eq!(unsafe { libc::mprotect(host as *mut _, page, libc::PROT_READ | libc::PROT_WRITE) }, 0);
                restored.write_obj(super::super::queue::Descriptor {
                    addr: page as u64, len: 8, flags: 0, next: 0,
                }, GuestAddress(0)).unwrap();
            } else if ranges == [(page as u64, 8)] {
                assert_eq!(unsafe { libc::mprotect((host + page) as *mut _, page, libc::PROT_READ | libc::PROT_WRITE) }, 0);
                restored.write_slice(b"restored", GuestAddress(page as u64)).unwrap();
            } else {
                panic!("unexpected preparation range: {ranges:?}");
            }
            Ok(())
        }))).unwrap();
        gate.open();
        let head = super::super::DescriptorChain::checked_new(&memory, GuestAddress(0), 1, 0).unwrap();
        let mut reader = super::super::descriptor_utils::Reader::new(&memory, head).unwrap();
        let mut bytes = [0; 8];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"restored");
        assert_eq!(*ranges_seen.lock().unwrap(), vec![vec![(0, 16)], vec![(page as u64, 8)]]);
        assert!(gate.close(Duration::ZERO).is_err());
        drop(reader);
        gate.close(Duration::ZERO).unwrap();
        gate.set_prepare(None).unwrap();
        gate.open();
    }

    #[test]
    fn prepare_runs_before_access_with_a_lease_and_without_gate_lock() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use vm_memory::GuestAddress;
        let memory = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 4096)]).unwrap();
        let gate = register(&memory);
        let count = Arc::new(AtomicUsize::new(0));
        let weak = Arc::downgrade(&gate);
        let calls = count.clone();
        let prepare: Arc<MemoryPrepare> = Arc::new(move |ranges| {
            assert!(ranges.is_empty());
            let gate = weak.upgrade().unwrap();
            // Reentering the gate lock must not deadlock; the live lease must
            // prevent close while memory preparation is still in progress.
            assert!(gate.close(Duration::ZERO).is_err());
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert!(gate.set_prepare(Some(prepare.clone())).is_err());
        gate.close(Duration::ZERO).unwrap();
        gate.set_prepare(Some(prepare)).unwrap();
        gate.open();
        let lease = access(&memory).unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(gate.set_prepare(None).is_err());
        drop(lease);
        gate.close(Duration::ZERO).unwrap();
        gate.set_prepare(None).unwrap();
        gate.open();
        drop(access(&memory));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn reader_retains_access_after_consuming_descriptors() {
        use vm_memory::{Bytes, GuestAddress};
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 8192)]).unwrap();
        let gate = register(&mem);
        let descriptor = super::super::queue::Descriptor {
            addr: 4096,
            len: 8,
            flags: 0,
            next: 0,
        };
        mem.write_obj(descriptor, GuestAddress(0)).unwrap();
        let chain =
            super::super::DescriptorChain::checked_new(&mem, GuestAddress(0), 1, 0).unwrap();
        let reader = super::super::descriptor_utils::Reader::new(&mem, chain).unwrap();
        assert!(gate.close(Duration::ZERO).is_err());
        drop(reader);
        gate.close(Duration::ZERO).unwrap();
        gate.open();
    }

    #[test]
    fn split_writer_retains_access_after_original_is_dropped() {
        use std::io::Write;
        use vm_memory::{Bytes, GuestAddress};
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 8192)]).unwrap();
        let gate = register(&mem);
        mem.write_obj(
            super::super::queue::Descriptor {
                addr: 4096,
                len: 8,
                flags: super::super::queue::VIRTQ_DESC_F_WRITE,
                next: 0,
            },
            GuestAddress(0),
        )
        .unwrap();
        let chain =
            super::super::DescriptorChain::checked_new(&mem, GuestAddress(0), 1, 0).unwrap();
        let mut writer = super::super::descriptor_utils::Writer::new(&mem, chain).unwrap();
        let mut tail = writer.split_at(4).unwrap();
        drop(writer);
        assert!(gate.close(Duration::ZERO).is_err());
        // A retained slice must be able to finish its operation during draining.
        tail.write_all(b"tail").unwrap();
        drop(tail);
        gate.close(Duration::ZERO).unwrap();
        let mut actual = [0; 4];
        mem.read_slice(&mut actual, GuestAddress(4100)).unwrap();
        assert_eq!(&actual, b"tail");
        gate.open();
    }

    #[test]
    fn closing_one_vm_does_not_block_another_vm() {
        use vm_memory::GuestAddress;
        let first = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 4096)]).unwrap();
        let second = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 4096)]).unwrap();
        let first_gate = register(&first);
        let second_gate = register(&second);
        first_gate.close(Duration::ZERO).unwrap();
        let _access = access(&second).unwrap();
        assert!(second_gate.close(Duration::ZERO).is_err());
        assert!(first_gate.is_idle_closed());
        first_gate.open();
        second_gate.open();
    }

    #[test]
    fn retained_access_prevents_close_and_new_access_waits_for_resume() {
        let gate = Arc::new(MemoryGate::default());
        let access = gate.enter();
        let clone = access.clone();
        drop(access);
        assert!(gate.close(Duration::ZERO).is_err());
        // Nested queue operations may finish while the outer access drains.
        let nested = gate.enter();
        drop(nested);
        drop(clone);
        gate.close(Duration::ZERO).unwrap();
        assert!(gate.is_idle_closed());
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = gate.clone();
        let thread = std::thread::spawn(move || {
            let _access = worker.enter();
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(20)).is_err());
        gate.open();
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        thread.join().unwrap();
    }
}
