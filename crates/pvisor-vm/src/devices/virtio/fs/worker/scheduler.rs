//! Queue-local execution with stealing directly from a blocked owner's ring.
//! Only descriptor admission and used-ring publication hold the ring mutex.
use super::*;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

const STEAL_DELAY: Duration = Duration::from_millis(2);

struct Wake(EventFd);

impl Default for Wake {
    fn default() -> Self {
        Self(EventFd::new(EFD_NONBLOCK).unwrap())
    }
}

impl Wake {
    fn signal(&self) {
        let _ = self.0.write(1);
    }
}

fn drain_event(event: &EventFd) {
    // Bounded for pipe-backed eventfds too; a continuous kick stream cannot
    // prevent observing stop or scanning descriptors.
    for _ in 0..64 {
        if event.read().is_err() {
            break;
        }
    }
}

fn heavy(opcode: u32) -> bool {
    matches!(opcode, x if x == Opcode::Read as u32 || x == Opcode::Write as u32
        || x == Opcode::Fsync as u32 || x == Opcode::Fsyncdir as u32
        || x == Opcode::Open as u32 || x == Opcode::Create as u32
        || x == Opcode::Setattr as u32 || x == Opcode::Rename as u32
        || x == Opcode::Rename2 as u32 || x == Opcode::Unlink as u32
        || x == Opcode::Rmdir as u32 || x == Opcode::Link as u32)
}

struct Lane {
    ring: Mutex<Queue>,
    busy: AtomicBool,
    started: AtomicU64,
    stealable: AtomicBool,
    wake: Wake,
}

struct Runtime {
    lanes: Vec<Lane>,
    epoch: Instant,
    stopped: AtomicBool,
    execution: Arc<Execution>,
    interrupt: InterruptTransport,
    profile: pvisor_overlay_core::profile::Profile,
}

impl Runtime {
    fn wake_thieves(&self, victim: usize) {
        for (index, lane) in self.lanes.iter().enumerate().skip(REQ_INDEX) {
            if index != victim
                && lane.ring.try_lock().is_ok_and(|ring| ring.ready)
                && !lane.busy.load(Ordering::Acquire)
            {
                lane.wake.signal();
                // The thief propagates a wake if backlog remains after its
                // claim, avoiding a herd of contenders for a single request.
                break;
            }
        }
    }

    fn long_running(&self, lane: &Lane) -> bool {
        let started = lane.started.load(Ordering::Acquire);
        started != 0
            && lane.busy.load(Ordering::Acquire)
            && (self.epoch.elapsed().as_micros() as u64 + 1).saturating_sub(started)
                >= STEAL_DELAY.as_micros() as u64
    }

    fn take(&self, owner: usize, victim: usize) -> Option<Request> {
        if self.stopped.load(Ordering::Acquire) {
            return None;
        }
        let lane = &self.lanes[victim];
        let mut ring = if owner == victim {
            // The owner must not sleep just because a thief briefly publishes
            // a completion: there may be no later kick for the remaining work.
            lane.ring.lock().unwrap()
        } else {
            lane.ring.try_lock().ok()?
        };
        // A guest may initialize fewer request queues than advertised.
        if !ring.ready || self.stopped.load(Ordering::Acquire) {
            return None;
        }
        if owner != victim && !self.long_running(lane) {
            return None;
        }
        let mem = &self.execution.mem;
        loop {
            if self.stopped.load(Ordering::Acquire) {
                return None;
            }
            ring.disable_notification(mem).unwrap();
            // The guest can publish between an empty pop and a subsequent
            // avail read. Classify malformed heads from the pre-pop state;
            // a later publication belongs to the notification recheck.
            let available = !ring.is_empty(mem);
            let head = if available { ring.pop(mem) } else { None };
            let Some(head) = head else {
                if available {
                    // Invalid descriptor heads must not spin forever and
                    // prevent device freeze from stopping admission.
                    error!("invalid filesystem available descriptor");
                    std::process::abort();
                }
                if ring.enable_notification(mem).unwrap() {
                    continue;
                }
                return None;
            };
            let index = head.index;
            let buffers = match OwnedDescriptorChain::new(head) {
                Ok(buffers) => buffers,
                Err(error) => {
                    error!("invalid filesystem descriptor chain: {error}");
                    ring.add_used(mem, index, 0).unwrap();
                    if ring.needs_notification(mem).unwrap() {
                        self.interrupt.signal_used_queue();
                    }
                    continue;
                }
            };
            let header = buffers
                .read_prefix::<InHeader>(mem)
                .map_err(super::super::FsError::QueueReader);
            let header = match header {
                Ok(header) => header,
                Err(error) => {
                    error!("invalid filesystem header: {error:?}");
                    ring.add_used(mem, index, 0).unwrap();
                    if ring.needs_notification(mem).unwrap() {
                        self.interrupt.signal_used_queue();
                    }
                    continue;
                }
            };
            if victim == HPQ_INDEX
                && !matches!(header.opcode,
                x if x == Opcode::Forget as u32
                    || x == Opcode::BatchForget as u32
                    || x == Opcode::Interrupt as u32)
            {
                error!("normal request submitted to filesystem hiprio queue");
                ring.add_used(mem, index, 0).unwrap();
                if ring.needs_notification(mem).unwrap() {
                    self.interrupt.signal_used_queue();
                }
                continue;
            }
            // Keep guest notifications suppressed while this queue is being
            // drained. Empty-ring admission above arms and rechecks before
            // an owner may sleep. Delayed rescue scans backlog independently
            // of guest kicks, including when the owner remains in a syscall.
            self.lanes[owner].busy.store(true, Ordering::Release);
            self.lanes[owner].stealable.store(false, Ordering::Release);
            self.lanes[owner].started.store(
                if owner != HPQ_INDEX && heavy(header.opcode) {
                    self.epoch.elapsed().as_micros() as u64 + 1
                } else {
                    0
                },
                Ordering::Release,
            );
            drop(ring);
            return Some(Request {
                queue: victim,
                index,
                header,
                buffers,
                #[cfg(test)]
                profile_timestamp: None,
            });
        }
    }

    fn next(&self, owner: usize, cursor: &mut usize) -> Option<Request> {
        if let Some(request) = self.take(owner, owner) {
            return Some(request);
        }
        if owner == HPQ_INDEX {
            return None;
        }
        // Rotate victims so an overloaded queue cannot monopolize idle lanes.
        let count = self.lanes.len() - REQ_INDEX;
        for _ in 0..count {
            let victim = REQ_INDEX + *cursor % count;
            *cursor = (*cursor + 1) % count;
            if victim != owner
                && self.lanes[victim].stealable.load(Ordering::Acquire)
                && self.lanes[victim].busy.load(Ordering::Acquire)
            {
                if let Some(request) = self.take(owner, victim) {
                    self.profile.add("stolen_requests", 1);
                    return Some(request);
                }
            }
        }
        None
    }

    fn run(&self, owner: usize, kick: &EventFd) {
        // Queue ownership includes waiting for the guest kick. There is no
        // coordinator-to-owner handoff in the ordinary request path.
        let epoll = Epoll::new().unwrap();
        for event in [kick, &self.lanes[owner].wake.0] {
            epoll
                .ctl(
                    ControlOperation::Add,
                    event.as_raw_fd(),
                    &EpollEvent::new(EventSet::IN, event.as_raw_fd() as u64),
                )
                .unwrap();
        }
        let mut events: [EpollEvent; 2] =
            std::array::from_fn(|_| EpollEvent::new(EventSet::empty(), 0));
        let mut cursor = owner;
        while !self.stopped.load(Ordering::Acquire) {
            if let Some(request) = self.next(owner, &mut cursor) {
                self.profile.add(
                    if owner == HPQ_INDEX {
                        "hiprio_requests"
                    } else {
                        "direct_requests"
                    },
                    1,
                );
                let len = self.execution.handle(&request);
                self.lanes[owner].started.store(0, Ordering::Release);
                self.publish(request, len);
                self.lanes[owner].stealable.store(false, Ordering::Release);
                self.lanes[owner].busy.store(false, Ordering::Release);
                continue;
            }
            if self.stopped.load(Ordering::Acquire) {
                break;
            }
            match epoll.wait(events.len(), -1, &mut events) {
                Ok(count) => {
                    for event in &events[..count] {
                        if event.fd() == kick.as_raw_fd() {
                            drain_event(kick);
                        } else {
                            drain_event(&self.lanes[owner].wake.0);
                        }
                    }
                }
                Err(error) => debug!("filesystem lane epoll wait: {error}"),
            }
        }
    }

    fn publish(&self, request: Request, len: usize) {
        let mut ring = self.lanes[request.queue].ring.lock().unwrap();
        ring.add_used(&self.execution.mem, request.index, len as u32)
            .unwrap();
        if ring.needs_notification(&self.execution.mem).unwrap() {
            self.interrupt.signal_used_queue();
        }
        // The request owns its RAM lease through used-ring publication.
        drop(ring);
        drop(request);
    }
}

pub(super) fn serve(
    queues: Vec<Queue>,
    events: &[Arc<EventFd>],
    stop: &EventFd,
    interrupt: InterruptTransport,
    execution: Arc<Execution>,
    profile: pvisor_overlay_core::profile::Profile,
) -> Vec<Queue> {
    assert!(queues.len() > REQ_INDEX && queues.len() == events.len());
    let runtime = Arc::new(Runtime {
        epoch: Instant::now(),
        lanes: queues
            .into_iter()
            .map(|ring| Lane {
                ring: Mutex::new(ring),
                busy: AtomicBool::new(false),
                started: AtomicU64::new(0),
                stealable: AtomicBool::new(false),
                wake: Wake::default(),
            })
            .collect(),
        stopped: AtomicBool::new(false),
        execution,
        interrupt,
        profile,
    });
    let epoll = Epoll::new().unwrap();
    epoll
        .ctl(
            ControlOperation::Add,
            stop.as_raw_fd(),
            &EpollEvent::new(EventSet::IN, stop.as_raw_fd() as u64),
        )
        .unwrap();
    let mut threads = Vec::new();
    for (owner, kick) in events.iter().enumerate() {
        if !runtime.lanes[owner].ring.lock().unwrap().ready {
            continue;
        }
        let runtime = runtime.clone();
        let kick = kick.clone();
        threads.push(
            thread::Builder::new()
                .name(if owner == HPQ_INDEX {
                    "fs hiprio".into()
                } else {
                    format!("fs queue {owner}")
                })
                .spawn(move || {
                    // A lost lane would leave requests unpublished. Never capture
                    // a snapshot after an execution or ring-publication panic.
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        runtime.run(owner, &kick)
                    }))
                    .is_err()
                    {
                        std::process::abort();
                    }
                })
                .unwrap_or_else(|_| std::process::abort()),
        );
    }
    // Control path only: stop and delayed rescue. Elapsed time is a bounded
    // heuristic for a long-running operation, not proof of kernel sleep. No
    // ordinary kick is consumed or forwarded here, and short requests never
    // trigger a thief wake. The 2 ms control timer is independent of I/O.
    let mut ready = [EpollEvent::new(EventSet::empty(), 0); 1];
    loop {
        let count = match epoll.wait(ready.len(), 2, &mut ready) {
            Ok(count) => count,
            Err(error) => {
                debug!("filesystem control epoll wait: {error}");
                continue;
            }
        };
        if count != 0 {
            drain_event(stop);
            runtime.stopped.store(true, Ordering::Release);
            for lane in &runtime.lanes {
                lane.wake.signal();
            }
            break;
        }
        // Rescue is optional polling, not new guest work. Inspecting an empty
        // virtqueue every 2 ms would revoke every idle offer and fault RAM back
        // in. Real kicks, pending work and completion keep their wake paths.
        if crate::devices::virtio::memory_gate::idle_gate(&runtime.execution.mem)
            .is_some_and(|idle| idle.defer_maintenance())
        {
            continue;
        }
        let now = runtime.epoch.elapsed().as_micros() as u64 + 1;
        for (index, lane) in runtime.lanes.iter().enumerate().skip(REQ_INDEX) {
            let started = lane.started.load(Ordering::Acquire);
            if started != 0
                && now.saturating_sub(started) >= STEAL_DELAY.as_micros() as u64
                && lane.busy.load(Ordering::Acquire)
            {
                lane.stealable.store(true, Ordering::Release);
                if lane
                    .ring
                    .try_lock()
                    .is_ok_and(|ring| ring.ready && !ring.is_empty(&runtime.execution.mem))
                {
                    runtime.wake_thieves(index);
                }
            }
        }
    }
    for thread in threads {
        thread.join().unwrap_or_else(|_| std::process::abort());
    }
    // Every accepted request has executed and published before queue ownership
    // returns to the frozen device. Pending guest descriptors stay in the ring.
    let runtime = Arc::try_unwrap(runtime)
        .ok()
        .expect("filesystem lane still running");
    runtime
        .lanes
        .into_iter()
        .map(|lane| lane.ring.into_inner().unwrap())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::virtio::queue::tests::VirtQueue;
    use vm_memory::{Bytes, GuestAddress};

    fn fill(queue: &VirtQueue<'_>, mem: &GuestMemoryMmap, address: u64, opcodes: &[Opcode]) {
        for (slot, opcode) in opcodes.iter().enumerate() {
            let input = address + slot as u64 * 0x200;
            let descriptor = slot * 2;
            queue.dtable[descriptor].addr.set(input);
            queue.dtable[descriptor]
                .len
                .set(std::mem::size_of::<InHeader>() as u32);
            queue.dtable[descriptor]
                .flags
                .set(crate::devices::virtio::queue::VIRTQ_DESC_F_NEXT);
            queue.dtable[descriptor].next.set((descriptor + 1) as u16);
            queue.dtable[descriptor + 1].addr.set(input + 0x100);
            queue.dtable[descriptor + 1].len.set(128);
            queue.dtable[descriptor + 1]
                .flags
                .set(crate::devices::virtio::queue::VIRTQ_DESC_F_WRITE);
            mem.write_obj(
                InHeader {
                    len: std::mem::size_of::<InHeader>() as u32,
                    opcode: *opcode as u32,
                    ..Default::default()
                },
                GuestAddress(input),
            )
            .unwrap();
            queue.avail.ring[slot].set(descriptor as u16);
        }
        queue.avail.idx.set(opcodes.len() as u16);
    }

    fn runtime(mem: GuestMemoryMmap, queues: Vec<Queue>) -> Runtime {
        let worker = super::super::tests::queue_worker(mem, queues);
        Runtime {
            epoch: Instant::now(),
            lanes: worker
                .queues
                .into_iter()
                .map(|ring| Lane {
                    ring: Mutex::new(ring),
                    busy: AtomicBool::new(false),
                    started: AtomicU64::new(0),
                    stealable: AtomicBool::new(false),
                    wake: Wake::default(),
                })
                .collect(),
            stopped: AtomicBool::new(false),
            execution: worker.execution,
            interrupt: worker.interrupt,
            profile: worker.dispatch_profile,
        }
    }

    #[test]
    fn draining_suppresses_kicks_until_empty_and_rearms_both_notification_modes() {
        for event_idx in [false, true] {
            let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
            let high = VirtQueue::new(GuestAddress(0), &mem, 8);
            let first = VirtQueue::new(GuestAddress(0x400), &mem, 8);
            fill(
                &first,
                &mem,
                0x4000,
                &[Opcode::Read, Opcode::Getattr, Opcode::Getattr],
            );
            first.avail.idx.set(1);
            let mut ordinary = first.create_queue();
            ordinary.set_event_idx(event_idx);
            let runtime = runtime(mem.clone(), vec![high.create_queue(), ordinary]);

            let request = runtime.next(1, &mut 0).unwrap();
            if event_idx {
                assert_eq!(first.used.event.get(), 0);
            } else {
                assert_eq!(first.used.flags.get(), 1); // VRING_USED_F_NO_NOTIFY
            }
            // A driver may add a descriptor while kicks are suppressed. The
            // owner must consume it directly after its current completion.
            first.avail.idx.set(2);
            runtime.publish(request, 16);
            runtime.lanes[1].busy.store(false, Ordering::Release);
            let next = runtime.next(1, &mut 0).unwrap();
            assert_eq!(next.index, 2);
            runtime.publish(next, 16);
            runtime.lanes[1].busy.store(false, Ordering::Release);
            assert!(runtime.next(1, &mut 0).is_none());
            if event_idx {
                assert_eq!(first.used.event.get(), 2);
            } else {
                assert_eq!(first.used.flags.get(), 0);
            }
            // Empty-ring rearming makes the next driver submission eligible
            // for a kick, and the next admission must find that request.
            first.avail.idx.set(3);
            let final_request = runtime.next(1, &mut 0).unwrap();
            assert_eq!(final_request.index, 4);
            runtime.publish(final_request, 16);
            assert_eq!(first.used.idx.get(), 3);
        }
    }

    #[test]
    fn concurrent_guest_publication_after_empty_checks_is_consumed() {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let high = VirtQueue::new(GuestAddress(0), &mem, 8);
        let first = VirtQueue::new(GuestAddress(0x400), &mem, 8);
        fill(&first, &mem, 0x4000, &[Opcode::Getattr]);
        first.avail.idx.set(0);
        for slot in 0..8 {
            first.avail.ring[slot].set(0);
        }
        let runtime = runtime(mem.clone(), vec![high.create_queue(), first.create_queue()]);
        let deadline = Instant::now() + Duration::from_secs(5);
        thread::scope(|scope| {
            let producer = scope.spawn(|| {
                for count in 0..5000u16 {
                    while mem
                        .load::<u16>(first.used.idx.location, Ordering::Acquire)
                        .unwrap()
                        != count
                    {
                        assert!(Instant::now() < deadline, "guest completion wait timed out");
                        thread::yield_now();
                    }
                    mem.store(count + 1, first.avail.idx.location, Ordering::Release)
                        .unwrap();
                }
            });
            for _ in 0..5000 {
                let request = loop {
                    if let Some(request) = runtime.next(1, &mut 0) {
                        break request;
                    }
                    assert!(Instant::now() < deadline, "guest publication was lost");
                    thread::yield_now();
                };
                runtime.publish(request, 16);
                runtime.lanes[1].busy.store(false, Ordering::Release);
            }
            producer.join().unwrap();
        });
        assert_eq!(first.used.idx.get(), 5000);
    }

    #[test]
    fn blocked_owner_backlog_is_stolen_and_completed_on_its_original_ring() {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let high = VirtQueue::new(GuestAddress(0), &mem, 8);
        let first = VirtQueue::new(GuestAddress(0x400), &mem, 8);
        let second = VirtQueue::new(GuestAddress(0x800), &mem, 8);
        fill(&first, &mem, 0x4000, &[Opcode::Read, Opcode::Getattr]);
        fill(&high, &mem, 0x6000, &[Opcode::Forget]);
        let gate = crate::devices::virtio::memory_gate::register(&mem);
        let runtime = runtime(
            mem.clone(),
            vec![
                high.create_queue(),
                first.create_queue(),
                second.create_queue(),
            ],
        );
        // Keep the first request outstanding, as if its owner were in a syscall.
        let blocked = runtime.next(1, &mut 0).unwrap();
        assert!(!gate.try_close().unwrap());
        assert!(runtime.lanes[1].ring.try_lock().is_ok());
        // A busy short request is not enough to admit stealing.
        assert!(runtime.next(2, &mut 0).is_none());
        std::thread::sleep(STEAL_DELAY);
        runtime.lanes[1].stealable.store(true, Ordering::Release);
        let stolen = runtime.next(2, &mut 0).unwrap();
        assert_eq!(stolen.queue, 1);
        assert_eq!(stolen.header.opcode, Opcode::Getattr as u32);
        runtime.publish(stolen, 16);
        assert!(!gate.try_close().unwrap());
        assert_eq!(first.used.idx.get(), 1);
        assert_eq!(second.used.idx.get(), 0);
        assert_eq!(first.used.ring[0].get().id, 2);
        assert!(runtime.next(2, &mut 0).is_none());
        // Neither busy ordinary execution nor stealing consumes hiprio.
        let control = runtime.next(0, &mut 0).unwrap();
        assert_eq!(control.header.opcode, Opcode::Forget as u32);
        runtime.publish(control, 0);
        runtime.publish(blocked, 32);
        assert_eq!(high.used.idx.get(), 1);
        assert_eq!(first.used.idx.get(), 2);
        assert!(gate.try_close().unwrap());
    }

    #[test]
    fn idle_owners_are_not_stolen_and_stop_preserves_unaccepted_descriptors() {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let high = VirtQueue::new(GuestAddress(0), &mem, 8);
        let first = VirtQueue::new(GuestAddress(0x400), &mem, 8);
        let second = VirtQueue::new(GuestAddress(0x800), &mem, 8);
        fill(&first, &mem, 0x4000, &[Opcode::Getattr]);
        let runtime = runtime(
            mem.clone(),
            vec![
                high.create_queue(),
                first.create_queue(),
                second.create_queue(),
            ],
        );
        assert!(runtime.next(2, &mut 0).is_none());
        runtime.stopped.store(true, Ordering::Release);
        assert!(runtime.next(1, &mut 0).is_none());
        assert_eq!(
            runtime.lanes[1]
                .ring
                .lock()
                .unwrap()
                .len(&runtime.execution.mem),
            1
        );
        assert_eq!(first.used.idx.get(), 0);
    }

    #[test]
    fn concurrent_thieves_claim_distinct_descriptors() {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let high = VirtQueue::new(GuestAddress(0), &mem, 8);
        let first = VirtQueue::new(GuestAddress(0x400), &mem, 8);
        let second = VirtQueue::new(GuestAddress(0x800), &mem, 8);
        let third = VirtQueue::new(GuestAddress(0xc00), &mem, 8);
        fill(
            &first,
            &mem,
            0x4000,
            &[
                Opcode::Read,
                Opcode::Getattr,
                Opcode::Getattr,
                Opcode::Getattr,
            ],
        );
        let runtime = runtime(
            mem.clone(),
            vec![
                high.create_queue(),
                first.create_queue(),
                second.create_queue(),
                third.create_queue(),
            ],
        );
        let blocked = runtime.next(1, &mut 0).unwrap();
        std::thread::sleep(STEAL_DELAY);
        runtime.lanes[1].stealable.store(true, Ordering::Release);
        let steal = |owner| {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut cursor = owner;
            loop {
                if let Some(request) = runtime.next(owner, &mut cursor) {
                    let index = request.index;
                    runtime.publish(request, 16);
                    return index;
                }
                assert!(Instant::now() < deadline, "thief failed to claim backlog");
                thread::yield_now();
            }
        };
        let mut claimed = thread::scope(|scope| {
            let a = scope.spawn(|| steal(2));
            let b = scope.spawn(|| steal(3));
            vec![a.join().unwrap(), b.join().unwrap()]
        });
        claimed.sort_unstable();
        assert_eq!(claimed, [2, 4]);
        runtime.publish(blocked, 32);
        assert_eq!(first.used.idx.get(), 3);
        assert_eq!(
            runtime.lanes[1]
                .ring
                .lock()
                .unwrap()
                .len(&runtime.execution.mem),
            1
        );
    }

    #[test]
    fn hiprio_rejects_normal_operations_and_never_steals_them() {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let high = VirtQueue::new(GuestAddress(0), &mem, 8);
        let first = VirtQueue::new(GuestAddress(0x400), &mem, 8);
        fill(&high, &mem, 0x4000, &[Opcode::Read]);
        fill(&first, &mem, 0x6000, &[Opcode::Getattr]);
        let runtime = runtime(mem.clone(), vec![high.create_queue(), first.create_queue()]);
        runtime.lanes[1].busy.store(true, Ordering::Release);
        assert!(runtime.next(0, &mut 0).is_none());
        assert_eq!(high.used.idx.get(), 1);
        assert_eq!(first.used.idx.get(), 0);
    }

    #[test]
    fn lane_consumes_its_kick_and_stops_without_a_forwarder() {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let high = VirtQueue::new(GuestAddress(0), &mem, 8);
        let first = VirtQueue::new(GuestAddress(0x400), &mem, 8);
        fill(&first, &mem, 0x4000, &[Opcode::Getattr]);
        first.avail.idx.set(0);
        let runtime = runtime(mem.clone(), vec![high.create_queue(), first.create_queue()]);
        let kick = EventFd::new(EFD_NONBLOCK).unwrap();
        thread::scope(|scope| {
            let lane = scope.spawn(|| runtime.run(1, &kick));
            mem.store(1u16, first.avail.idx.location, Ordering::Release)
                .unwrap();
            kick.write(1).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while mem
                .load::<u16>(first.used.idx.location, Ordering::Acquire)
                .unwrap()
                != 1
            {
                if Instant::now() >= deadline {
                    runtime.stopped.store(true, Ordering::Release);
                    runtime.lanes[1].wake.signal();
                    panic!("direct queue kick was not serviced");
                }
                thread::yield_now();
            }
            runtime.stopped.store(true, Ordering::Release);
            runtime.lanes[1].wake.signal();
            lane.join().unwrap();
        });
    }

    #[test]
    fn short_operations_are_not_stealable_even_after_the_delay() {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let high = VirtQueue::new(GuestAddress(0), &mem, 8);
        let first = VirtQueue::new(GuestAddress(0x400), &mem, 8);
        let second = VirtQueue::new(GuestAddress(0x800), &mem, 8);
        fill(&first, &mem, 0x4000, &[Opcode::Getattr, Opcode::Getattr]);
        let runtime = runtime(
            mem.clone(),
            vec![
                high.create_queue(),
                first.create_queue(),
                second.create_queue(),
            ],
        );
        let short = runtime.next(1, &mut 0).unwrap();
        thread::sleep(STEAL_DELAY);
        runtime.lanes[1].stealable.store(true, Ordering::Release); // stale rescue hint
        assert!(runtime.next(2, &mut 0).is_none());
        runtime.publish(short, 16);
        assert_eq!(runtime.lanes[1].ring.lock().unwrap().len(&mem), 1);
    }

    #[test]
    fn wake_before_wait_is_retained() {
        let wake = Wake::default();
        wake.signal();
        let epoll = Epoll::new().unwrap();
        epoll
            .ctl(
                ControlOperation::Add,
                wake.0.as_raw_fd(),
                &EpollEvent::new(EventSet::IN, wake.0.as_raw_fd() as u64),
            )
            .unwrap();
        let mut events = [EpollEvent::new(EventSet::empty(), 0)];
        assert_eq!(epoll.wait(1, 100, &mut events).unwrap(), 1);
        drain_event(&wake.0);
        assert_eq!(epoll.wait(1, 0, &mut events).unwrap(), 0);
    }
}
