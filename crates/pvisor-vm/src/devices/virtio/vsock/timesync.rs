use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time;

use super::super::Queue as VirtQueue;
use super::defs::uapi;
use super::packet::VsockPacket;

use crate::devices::virtio::InterruptTransport;
use vm_memory::GuestMemoryMmap;

const UPDATE_INTERVAL: u64 = 60 * 1000 * 1000 * 1000;
const SLEEP_NSECS: u64 = 2 * 1000 * 1000 * 1000;
const TSYNC_PORT: u32 = 123;

pub struct TimesyncThread {
    stop: Arc<AtomicBool>,
    last_update: u64,
    last_awake: u64,
    cid: u64,
    mem: GuestMemoryMmap,
    queue_mutex: Arc<Mutex<VirtQueue>>,
    interrupt: InterruptTransport,
}

impl TimesyncThread {
    pub fn new(
        stop: Arc<AtomicBool>,
        cid: u64,
        mem: GuestMemoryMmap,
        queue_mutex: Arc<Mutex<VirtQueue>>,
        interrupt: InterruptTransport,
    ) -> Self {
        Self {
            stop,
            last_update: 0,
            last_awake: crate::utils::time::get_time(crate::utils::time::ClockType::Real),
            cid,
            mem,
            queue_mutex,
            interrupt,
        }
    }

    fn send_time(&self, time: u64) {
        let mut queue = self.queue_mutex.lock().unwrap();
        if let Some(head) = queue.pop(&self.mem) {
            if let Ok(mut pkt) = VsockPacket::from_rx_virtq_head(&head) {
                if pkt.buf().is_none_or(|buf| buf.len() < 8) {
                    queue.undo_pop();
                    return;
                }
                pkt.set_op(uapi::VSOCK_OP_RW)
                    .set_src_cid(uapi::VSOCK_HOST_CID)
                    .set_dst_cid(self.cid)
                    .set_src_port(TSYNC_PORT)
                    .set_dst_port(TSYNC_PORT)
                    .set_type(uapi::VSOCK_TYPE_DGRAM);

                pkt.write_time_sync(time);
                pkt.set_len(8);
                if let Err(e) =
                    queue.add_used(&self.mem, head.index, pkt.hdr().len() as u32 + pkt.len())
                {
                    error!("failed to add used elements to the queue: {e:?}");
                }
                self.interrupt.signal_used_queue();
            } else {
                queue.undo_pop();
            }
        }
    }

    fn work(mut self) -> Self {
        loop {
            if self.stop.load(Ordering::Acquire) {
                return self;
            }
            let now = crate::utils::time::get_time(crate::utils::time::ClockType::Real);
            /*
             * We send a time sync packet if we slept for 3 times more
             * nanoseconds than expected (which is an indication the
             * system forced us to take a long nap), or if UPDATE_INTERVAL
             * has been reached.
             */
            if now.saturating_sub(self.last_awake) >= (SLEEP_NSECS * 3)
                || now.saturating_sub(self.last_update) >= UPDATE_INTERVAL
            {
                self.send_time(now);
                self.last_update = now;
            }

            self.last_awake = crate::utils::time::get_time(crate::utils::time::ClockType::Real);
            thread::park_timeout(time::Duration::from_nanos(SLEEP_NSECS));
        }
    }

    pub fn run(self) -> JoinHandle<Self> {
        thread::Builder::new()
            .name("vsock timesync".into())
            .spawn(move || self.work())
            .unwrap()
    }
}
