"""Exercise patch 0059's two notification functions without building a kernel."""

import unittest
from pathlib import Path

from test_local_flush_contract import function
from test_writeback_pipeline_contract import compile_and_run


class InlineGetattrTests(unittest.TestCase):
    def test_doorbell_fallback_and_completion_ownership(self):
        patch = Path(__file__).resolve().parents[1] / "patches/0059-virtio-fs-inline-getattr.patch"
        added = "\n".join(line[1:] for line in patch.read_text().splitlines()
                          if line.startswith("+") and not line.startswith("+++"))
        code = "\n".join((
            next(line for line in added.splitlines() if line.startswith("#define PVISOR_FS_F_INLINE_METADATA")),
            function(added, "static bool vm_notify_metadata("),
            function(added, "static void virtio_fs_kick_and_unlock("),
        ))
        compile_and_run(self, STUBS + code + CASES)


STUBS = r"""
#include <assert.h>
#include <stdbool.h>
#include <stddef.h>

#define BIT(n) (1U << (n))
#define VIRTIO_MMIO_QUEUE_NOTIFY 0x50
#define FUSE_GETATTR 3
#define FR_BACKGROUND 0
#define FR_ASYNC 1
#define __releases(lock)
struct virtqueue;
struct config { bool (*pvisor_notify_metadata)(struct virtqueue *); };
struct virtio_mmio_device { struct config *config; bool pvisor_fs_mmio; unsigned features, base; };
struct virtqueue { struct virtio_mmio_device *vdev; unsigned index; bool broken; };
struct fuse_pqueue { bool lock; };
struct fuse_dev { struct fuse_pqueue pq; };
struct virtio_fs_vq { struct virtqueue *vq; struct fuse_dev *fud; int in_flight; bool lock; };
struct args { bool may_block; };
struct fuse_req {
    struct { struct { int opcode; } h; } in;
    unsigned flags;
    struct args *args;
    bool list;
};
#define to_virtio_mmio_device(vdev) (vdev)
#define __virtio_test_bit(vdev, bit) ((vdev)->features & BIT(bit))
#define virtqueue_is_broken(vq) ((vq)->broken)
#define test_bit(bit, ptr) ((*(ptr) & BIT(bit)) != 0)

static struct virtio_fs_vq lane;
static struct fuse_req request;
static unsigned cpus, ordinary, flagged, probes, ended;
static bool need_kick, used;
static unsigned num_online_cpus(void) { return cpus; }
static void spin_lock(bool *lock) { assert(!*lock); *lock = true; }
static void spin_unlock(bool *lock) { assert(*lock); *lock = false; }
static void list_del_init(bool *list) {
    assert(lane.lock && lane.fud->pq.lock && *list);
    *list = false;
}
static bool virtqueue_kick_prepare(struct virtqueue *vq) {
    assert(lane.lock && vq == lane.vq); return need_kick;
}
static void writel(unsigned value, unsigned reg) {
    assert(lane.lock && reg == VIRTIO_MMIO_QUEUE_NOTIFY && value == (BIT(31) | 1));
    flagged++;
}
static void virtqueue_notify(struct virtqueue *vq) { assert(!lane.lock); ordinary++; }
static void *virtqueue_get_buf(struct virtqueue *vq, unsigned *len) {
    assert(lane.lock && flagged); probes++;
    if (!used) return NULL;
    used = false; *len = 120; return &request;
}
static void virtio_fs_request_complete(struct fuse_req *r, struct virtio_fs_vq *q) {
    assert(r == &request && !r->list && !q->lock && !q->fud->pq.lock);
    ended++; q->in_flight--;
}
"""

CASES = r"""
int main(void) {
    enum { INLINE, WORKER_EARLY, WORKER_LATE, UNSUPPORTED, UNNEGOTIATED, OTHER_TRANSPORT, BROKEN,
           WRITE, BUSY, BACKGROUND, ASYNC, MAY_BLOCK, RETRY, SMP, SUPPRESSED };
    for (int c = INLINE; c <= SUPPRESSED; c++) {
        struct config ops = { c == OTHER_TRANSPORT ? NULL : vm_notify_metadata };
        struct virtio_mmio_device dev = {
            &ops, c != UNSUPPORTED, c == UNNEGOTIATED ? 0 : BIT(23), 0
        };
        struct virtqueue vq = { &dev, 1, c == BROKEN };
        struct fuse_dev fud = {0};
        struct args args = { c == MAY_BLOCK };
        int outstanding = c == BUSY ? 2 : 1;
        lane = (struct virtio_fs_vq){ &vq, &fud, outstanding, true };
        request = (struct fuse_req){ .in.h.opcode = c == WRITE ? 16 : FUSE_GETATTR,
            .flags = (c == BACKGROUND ? BIT(FR_BACKGROUND) : 0) |
                     (c == ASYNC ? BIT(FR_ASYNC) : 0), .args = &args, .list = true };
        cpus = c == SMP ? 2 : 1;
        flagged = probes = ordinary = ended = 0;
        used = c == INLINE || c == WORKER_EARLY; need_kick = c != SUPPRESSED;
        virtio_fs_kick_and_unlock(&lane, &request, c == RETRY);
        assert(!lane.lock && !fud.pq.lock && !used);
        assert(flagged == (c <= WORKER_LATE) && probes == flagged);
        assert(ordinary == (!flagged && need_kick));
        assert(ended == (c == INLINE || c == WORKER_EARLY) && request.list == !ended);
        assert(lane.in_flight == outstanding - (int)ended);
        if (c == WORKER_EARLY || c == WORKER_LATE) {
            /* A worker publishes used and signals; its scheduled drain takes lane.lock. */
            used = c == WORKER_LATE;
            unsigned len;
            spin_lock(&lane.lock);
            struct fuse_req *r = virtqueue_get_buf(&vq, &len);
            if (r) {
                spin_lock(&fud.pq.lock);
                list_del_init(&r->list);
                spin_unlock(&fud.pq.lock);
            }
            spin_unlock(&lane.lock);
            if (r) virtio_fs_request_complete(r, &lane);
            assert(ended == 1 && !request.list && !lane.in_flight);
        }
    }
    return 0;
}
"""


if __name__ == "__main__":
    unittest.main()
