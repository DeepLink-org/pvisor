// SPDX-License-Identifier: MPL-2.0

//! Virtio-fs request queue management.
//!
//! This module defines [`FsRequestQueue`], which submits [`FuseRequest`] values
//! to a virtqueue, tracks in-flight requests, and completes replies outside the
//! IRQ-disabled queue lock.

use alloc::{sync::Arc, vec::Vec};
use core::mem;

use aster_fuse::{FuseError, FuseOpcode};
use aster_softirq::{BottomHalfDisabled, Taskless};
use aster_util::{mem_obj_slice::Slice, slot_vec::SlotVec};
use dma_pool::DmaBuffer;
use ostd::{
    mm::dma::{FromDevice, ToDevice},
    sync::{LocalIrqDisabled, SpinGuardian, SpinLock, WaitQueue},
};
use smallvec::SmallVec;
use spin::Once;

use super::request::FuseRequest;
use crate::queue::VirtQueue;

/// Maximum virtqueue descriptors used by one FUSE request.
///
/// A request may contain a request header, multiple read/write payload buffers,
/// a reply header, and a write reply payload. Operation-specific limits reserve
/// descriptors for these fixed parts and keep one request from exhausting a
/// queue.
pub(crate) const MAX_DMA_BUFS_PER_REQUEST: usize = 64;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum SubmitMode {
    Interrupt,
    InlineMetadata,
    Handoff,
}

/// A virtio-fs request queue and its in-flight request state.
pub(super) struct FsRequestQueue {
    inner: SpinLock<FsRequestQueueInner, LocalIrqDisabled>,
    wait_queue: WaitQueue,
    taskless: Once<Arc<Taskless>>,
    mode: SubmitMode,
}

struct FsRequestQueueInner {
    queue: VirtQueue,
    in_flight_requests: SlotVec<FuseRequest>,
    pending_completions: Vec<CompletedFuseRequest>,
}

struct CompletedFuseRequest {
    request: FuseRequest,
    reply_len: usize,
}

impl FsRequestQueue {
    /// Creates a request queue backed by a virtqueue.
    pub(super) fn new(queue: VirtQueue, mode: SubmitMode) -> Arc<Self> {
        Arc::new(Self {
            inner: SpinLock::new(FsRequestQueueInner {
                queue,
                in_flight_requests: SlotVec::new(),
                pending_completions: Vec::new(),
            }),
            wait_queue: WaitQueue::new(),
            taskless: Once::new(),
            mode,
        })
    }

    /// Initializes the deferred completion handler for this queue.
    pub(super) fn init_completion_taskless(self: &Arc<Self>) {
        let queue_weak = Arc::downgrade(self);

        self.taskless.call_once(|| {
            Taskless::new(move || {
                let Some(queue) = queue_weak.upgrade() else {
                    return;
                };

                let completions = {
                    let mut inner = queue.inner.lock();
                    mem::take(&mut inner.pending_completions)
                };
                for completion in completions {
                    completion.request.finish_reply(completion.reply_len);
                }
            })
        });
    }

    /// Submits a FUSE request to the virtqueue.
    pub(super) fn add_request(
        &self,
        request: FuseRequest,
        opcode: FuseOpcode,
        deferred: bool,
    ) -> Result<(), FuseError> {
        let num_dma_bufs = request.num_dma_bufs();
        debug_assert!(num_dma_bufs <= MAX_DMA_BUFS_PER_REQUEST);

        let mut request = Some(request);

        let completed = self.wait_queue.wait_until(|| {
            let mut inner = self.inner.lock();
            if num_dma_bufs > inner.queue.available_desc() {
                return None;
            }

            let alone = inner.in_flight_requests.is_empty();
            let metadata = matches!(opcode, FuseOpcode::Lookup | FuseOpcode::Getattr);
            let inline = alone && self.mode == SubmitMode::InlineMetadata && metadata && !deferred;
            let poll = alone
                && self.mode == SubmitMode::Handoff
                && (metadata || opcode == FuseOpcode::Write);
            if poll {
                inner.queue.disable_callback();
            }
            let request = request.take().unwrap();
            let token = {
                let request_bufs = request
                    .request_bufs()
                    .iter()
                    .map(|buf| buf.as_dma_slice())
                    .collect::<SmallVec<[&Slice<DmaBuffer<ToDevice>>; 2]>>();
                let reply_bufs = request
                    .waiter()
                    .reply_bufs()
                    .iter()
                    .map(|buf| buf.as_dma_slice())
                    .collect::<SmallVec<[&Slice<DmaBuffer<FromDevice>>; 2]>>();

                inner
                    .queue
                    .add_dma_bufs(request_bufs.as_slice(), reply_bufs.as_slice())
                    .unwrap()
            };
            inner.in_flight_requests.put_at(token as usize, request);

            if !(inline && inner.queue.notify_inline_metadata()) {
                if inner.queue.should_notify() {
                    inner.queue.notify();
                }
                if !poll {
                    return Some(None);
                }
                for _ in 0..512 {
                    if inner.queue.can_pop() {
                        break;
                    }
                    core::hint::spin_loop();
                }
                // Arm first; pop_used/can_pop fences before rereading used.idx.
                inner.queue.enable_callback();
            }
            Some(inner.queue.pop_used().ok().map(|(used, len)| {
                debug_assert_eq!(used, token);
                CompletedFuseRequest {
                    request: inner.in_flight_requests.remove(used as usize).unwrap(),
                    reply_len: len as usize,
                }
            }))
        });

        if let Some(completed) = completed {
            self.wait_queue.wake_all();
            if deferred {
                // In task context, releasing the BH guard dispatches pending softirqs.
                // The Taskless callback must not depend on a future hardware IRQ.
                let _bh = BottomHalfDisabled::guard();
                self.inner.lock().pending_completions.push(completed);
                self.schedule_completion_taskless();
            } else {
                completed.request.finish_reply(completed.reply_len);
            }
        }
        Ok(())
    }

    /// Moves completed virtqueue descriptors into the pending-completion list.
    pub(super) fn drain_completed_requests(&self) {
        let mut inner = self.inner.lock();
        let mut has_freed_descs = false;
        while let Ok((token, len)) = inner.queue.pop_used() {
            has_freed_descs = true;
            let reply_len = len as usize;
            let request = inner.in_flight_requests.remove(token as usize).unwrap();
            inner
                .pending_completions
                .push(CompletedFuseRequest { request, reply_len });
        }
        drop(inner);

        if has_freed_descs {
            self.wait_queue.wake_all();
        }
    }

    /// Schedules deferred completion processing for this queue.
    pub(super) fn schedule_completion_taskless(&self) {
        self.taskless.get().unwrap().schedule();
    }
}
