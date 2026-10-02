//! VM-local barrier for device RAM access, including retained descriptor slices.
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};
use vm_memory::{GuestMemory, GuestMemoryMmap};

#[derive(Default)]
struct State {
    closed: bool,
    active: usize,
}
#[derive(Default)]
pub struct MemoryGate {
    state: Mutex<State>,
    changed: Condvar,
}
pub struct Access {
    gate: Arc<MemoryGate>,
}

impl MemoryGate {
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
    fn enter(self: &Arc<Self>) -> Arc<Access> {
        let mut state = self.state.lock().unwrap();
        while state.closed && state.active == 0 {
            state = self.changed.wait(state).unwrap();
        }
        state.active += 1;
        Arc::new(Access { gate: self.clone() })
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
pub(crate) fn access(mem: &GuestMemoryMmap) -> Option<Arc<Access>> {
    let gate = registry()
        .lock()
        .unwrap()
        .get(&key(mem))
        .and_then(Weak::upgrade);
    gate.map(|gate| gate.enter())
}

#[cfg(test)]
mod tests {
    use super::*;
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
