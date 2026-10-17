"""Run 0049's actual virtio-fs SG ownership and enqueue functions with stubs.

Descriptor construction is observed while the VQ lock is held. The fake
virtqueue consumes descriptors synchronously and retains only request-owned
buffers, matching the scratch lifetime contract. This does not model real DMA,
kernel reclaim or concurrent teardown; those require firmware/guest checks.
"""

import re
import unittest
from pathlib import Path

import test_local_flush_contract as extract
import test_writeback_pipeline_contract as harness


PATCH = Path(__file__).resolve().parents[1] / "patches/0049-fuse-coalesce-writeback-kicks.patch"


def candidate():
    text = PATCH.read_text()
    relative = "fs/fuse/virtio_fs.c"
    start = text.index(f"diff --git a/{relative} b/{relative}\n")
    end = text.find("\ndiff --git ", start + 1)
    lines, in_hunk = [], False
    for line in text[start:end if end >= 0 else len(text)].splitlines():
        if line.startswith("@@"):
            in_hunk = True
        elif in_hunk and line.startswith((" ", "+")):
            lines.append(line[1:])
    return "\n".join(lines)


def definition(source, name):
    # Enqueue also has an earlier prototype. Match the opening brace of the
    # definition, never the next unrelated function after that prototype.
    match = re.search(r"static (?:unsigned int|int|void) " + name + r"\([^;{]*?\)\n\{", source)
    if match is None:
        raise ValueError("missing actual function: " + name)
    return extract.function(source, match.group())


class VirtioSgScratchContractTests(unittest.TestCase):
    def test_actual_allocation_release_and_locked_enqueue_ownership(self):
        source = candidate()
        macro = re.search(r"^#define VIRTIO_FS_MAX_SGS\s+.*$", source, re.M).group()
        functions = (
            "virtio_fs_alloc_sg_scratch", "virtio_fs_free_sg_scratch",
            "virtio_fs_ktype_release", "copy_args_to_argbuf", "copy_args_from_argbuf",
            "sg_count_fuse_pages", "sg_count_fuse_req", "sg_init_fuse_pages",
            "sg_init_fuse_args", "virtio_fs_enqueue_req",
        )
        actual = "\n\n".join(definition(source, name) for name in functions)
        harness.compile_and_run(self, PRELUDE + macro + "\n" + actual + CASES)


PRELUDE = r"""
#include <assert.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>

#define FUSE_MAX_MAX_PAGES 256
#define VQ_REQUEST 1
#define GFP_KERNEL 1
#define GFP_ATOMIC 2
#define FR_ISREPLY 0
#define FR_SENT 1
#define ARRAY_SIZE(value) (sizeof(value) / sizeof((value)[0]))
#define min(a, b) ((a) < (b) ? (a) : (b))
#define container_of(ptr, type, member) ((type *)((char *)(ptr) - offsetof(type, member)))
#define smp_mb__after_atomic() do { } while (0)
static unsigned warnings;
#define WARN_ON_ONCE(condition) ({ bool value = !!(condition); if (value) warnings++; value; })
#define WARN_ON(condition) WARN_ON_ONCE(condition)

struct page { unsigned id; };
struct scatterlist { void *buffer; struct page *page; unsigned length, offset; };
struct list_head { unsigned entries; };
struct kobject { int unused; };
struct fuse_arg { unsigned size; void *value; };
struct fuse_args {
    unsigned in_numargs, out_numargs;
    bool in_pages, out_pages, out_argvar;
    struct fuse_arg in_args[2], out_args[2];
};
struct fuse_page_desc { unsigned length, offset; };
struct fuse_args_pages {
    struct fuse_args args;
    struct page **pages;
    struct fuse_page_desc *descs;
    unsigned num_pages;
};
struct header { unsigned len, tag; };
struct fuse_req {
    struct fuse_args *args;
    void *argbuf;
    struct { struct header h; } in, out;
    unsigned long flags;
    struct list_head list;
};
struct fuse_pqueue { int lock; struct list_head *processing; };
struct fuse_dev { struct fuse_pqueue pq; };
struct virtio_fs_vq;
struct virtqueue { struct virtio_fs_vq *owner; };
struct virtio_fs_vq {
    int lock;
    struct virtqueue *vq;
    struct fuse_dev *fud;
    struct scatterlist *sg_scratch;
    struct scatterlist **sgs_scratch;
    bool connected;
    long in_flight;
};
struct virtio_fs {
    struct kobject kobj;
    struct virtio_fs_vq *vqs;
    unsigned nvqs;
    void *mq_map;
};

struct allocation { void *pointer; bool live, scratch; };
static struct allocation allocations[1024];
static unsigned allocation_count, live_allocations, scratch_attempts, fail_scratch;
static unsigned scratch_frees, argument_allocations, argument_frees, locks;
static bool fail_argument;
static struct virtio_fs_vq *observed_vq;
static struct fuse_dev device;
static struct list_head processing;
static struct virtqueue queues[4];
static int add_error;
static unsigned adds, notifications, last_count;
static bool kick_notify;
static struct scatterlist *last_sg_base;
static void *last_argument;

static void *allocate(size_t bytes, bool scratch) {
    void *pointer = calloc(1, bytes ? bytes : 1);
    assert(pointer && allocation_count < ARRAY_SIZE(allocations));
    allocations[allocation_count++] = (struct allocation){ pointer, true, scratch };
    live_allocations++;
    return pointer;
}
static void release(void *pointer, bool scratch) {
    if (!pointer) return;
    assert(!locks);
    for (unsigned i = allocation_count; i > 0; i--) {
        struct allocation *entry = &allocations[i - 1];
        if (entry->pointer == pointer && entry->live) {
            assert(entry->scratch == scratch);
            entry->live = false; live_allocations--;
            if (scratch) scratch_frees++;
            free(pointer);
            return;
        }
    }
    assert(!"unowned or double free");
}
static void *kvcalloc(size_t count, size_t size, int flags) {
    assert(flags == GFP_KERNEL && !locks);
    scratch_attempts++;
    if (scratch_attempts == fail_scratch) return NULL;
    return allocate(count * size, true);
}
static void kvfree(void *pointer) { release(pointer, true); }
static void *kmalloc(size_t size, int flags) {
    /* Only per-request collapsed arguments may allocate at submission. */
    assert(flags == GFP_ATOMIC && !locks && size <= 64);
    argument_allocations++;
    return fail_argument ? NULL : allocate(size, false);
}
static void kfree(void *pointer) {
    if (pointer && pointer == last_argument) argument_frees++;
    release(pointer, false);
}
static bool test_bit(unsigned bit, const unsigned long *flags) { return !!(*flags & (1UL << bit)); }
static void set_bit(unsigned bit, unsigned long *flags) { *flags |= 1UL << bit; }
static void spin_lock(int *lock) { assert(!*lock); *lock = 1; locks++; }
static void spin_unlock(int *lock) { assert(*lock && locks); *lock = 0; locks--; }
static void check_vq_lock(void) { assert(observed_vq && observed_vq->lock); }
static unsigned fuse_len_args(unsigned count, struct fuse_arg *args) {
    unsigned length = 0;
    for (unsigned i = 0; i < count; i++) length += args[i].size;
    return length;
}
static void sg_init_one(struct scatterlist *sg, void *buffer, unsigned length) {
    check_vq_lock();
    *sg = (struct scatterlist){ .buffer = buffer, .length = length };
}
static void sg_init_table(struct scatterlist *sg, unsigned count) {
    check_vq_lock(); assert(count == 1); memset(sg, 0, sizeof(*sg));
}
static void sg_set_page(struct scatterlist *sg, struct page *page, unsigned length, unsigned offset) {
    check_vq_lock(); sg->page = page; sg->length = length; sg->offset = offset;
}
static int virtqueue_add_sgs(struct virtqueue *vq, struct scatterlist **sgs,
                            unsigned out, unsigned in, struct fuse_req *req, int flags) {
    assert(vq->owner == observed_vq && flags == GFP_ATOMIC);
    check_vq_lock(); adds++; last_count = out + in; last_sg_base = sgs[0];
    for (unsigned i = 0; i < last_count; i++) {
        assert(sgs[i] == &sgs[0][i] && sgs[i]->length);
    }
    assert(sgs[1]->buffer == req->argbuf);
    last_argument = req->argbuf;
    /* Real virtqueue_add_sgs consumes these lists here, not at completion. */
    return add_error;
}
static void list_add_tail(struct list_head *entry, struct list_head *head) {
    check_vq_lock(); assert(device.pq.lock); entry->entries++; head->entries++;
}
static void inc_in_flight_req(struct virtio_fs_vq *vq) { check_vq_lock(); vq->in_flight++; }
static bool virtqueue_kick_prepare(struct virtqueue *vq) { check_vq_lock(); return kick_notify; }
static void virtqueue_notify(struct virtqueue *vq) { assert(!locks); notifications++; }
"""


CASES = r"""
struct fixture {
    struct fuse_req req;
    struct fuse_args_pages ap;
    struct page pages[257];
    struct page *page_ptrs[257];
    struct fuse_page_desc descs[257];
    unsigned char input[16], output[8];
};
static void reset(void) {
    assert(!live_allocations && !locks);
    allocation_count = scratch_attempts = fail_scratch = scratch_frees = 0;
    argument_allocations = argument_frees = warnings = adds = notifications = 0;
    last_argument = NULL; last_sg_base = NULL; observed_vq = NULL;
    fail_argument = false; add_error = 0; kick_notify = true;
    memset(&device, 0, sizeof(device)); memset(&processing, 0, sizeof(processing));
    device.pq.processing = &processing;
}
static struct virtio_fs *make_fs(unsigned nvqs) {
    struct virtio_fs *fs = allocate(sizeof(*fs), false);
    fs->nvqs = nvqs;
    if (nvqs) fs->vqs = allocate(nvqs * sizeof(*fs->vqs), false);
    fs->mq_map = allocate(16, false);
    for (unsigned i = 0; i < nvqs; i++) {
        queues[i].owner = &fs->vqs[i]; fs->vqs[i].vq = &queues[i];
        fs->vqs[i].fud = &device; fs->vqs[i].connected = true;
    }
    return fs;
}
static void request(struct fixture *fixture, unsigned input_pages, unsigned output_pages, unsigned char byte) {
    memset(fixture, 0, sizeof(*fixture));
    fixture->req.args = &fixture->ap.args;
    fixture->req.flags = 1UL << FR_ISREPLY;
    fixture->ap.num_pages = input_pages > output_pages ? input_pages : output_pages;
    fixture->ap.pages = fixture->page_ptrs; fixture->ap.descs = fixture->descs;
    for (unsigned i = 0; i < fixture->ap.num_pages; i++) {
        fixture->pages[i].id = i; fixture->page_ptrs[i] = &fixture->pages[i];
        fixture->descs[i] = (struct fuse_page_desc){ .length = 4096, .offset = 0 };
    }
    memset(fixture->input, byte, sizeof(fixture->input));
    struct fuse_args *args = &fixture->ap.args;
    args->in_numargs = 1 + !!input_pages; args->in_pages = !!input_pages;
    args->out_numargs = 1 + !!output_pages; args->out_pages = !!output_pages;
    args->in_args[0] = (struct fuse_arg){ sizeof(fixture->input), fixture->input };
    args->out_args[0] = (struct fuse_arg){ sizeof(fixture->output), fixture->output };
    if (input_pages) args->in_args[1].size = input_pages * 4096;
    if (output_pages) args->out_args[1].size = output_pages * 4096;
    fixture->req.out.h.len = sizeof(fixture->req.out.h) + sizeof(fixture->output)
        + output_pages * 4096;
}
static void complete(struct fixture *fixture, unsigned char reply) {
    memset(fixture->req.argbuf + sizeof(fixture->input), reply, sizeof(fixture->output));
    copy_args_from_argbuf(&fixture->ap.args, &fixture->req);
    assert(!fixture->req.argbuf);
    for (unsigned i = 0; i < sizeof(fixture->output); i++) assert(fixture->output[i] == reply);
}
static void test_partial_allocation_and_final_owner(void) {
    for (unsigned failure = 1; failure <= 4; failure++) {
        reset(); struct virtio_fs *fs = make_fs(3); fail_scratch = failure;
        assert(virtio_fs_alloc_sg_scratch(fs) == -ENOMEM);
        assert(!fs->vqs[0].sg_scratch && !fs->vqs[0].sgs_scratch);
        virtio_fs_ktype_release(&fs->kobj);
        assert(!live_allocations && scratch_frees == failure - 1);
    }
    reset(); struct virtio_fs *fs = make_fs(3);
    assert(virtio_fs_alloc_sg_scratch(fs) == 0 && scratch_attempts == 4);
    assert(fs->vqs[1].sg_scratch != fs->vqs[2].sg_scratch);
    assert(fs->vqs[1].sgs_scratch != fs->vqs[2].sgs_scratch);
    virtio_fs_free_sg_scratch(fs);
    assert(scratch_frees == 4);
    for (unsigned i = 0; i < fs->nvqs; i++) assert(!fs->vqs[i].sg_scratch && !fs->vqs[i].sgs_scratch);
    virtio_fs_free_sg_scratch(fs); assert(scratch_frees == 4);
    virtio_fs_ktype_release(&fs->kobj); assert(!live_allocations && scratch_frees == 4);
    reset(); fs = make_fs(0); virtio_fs_free_sg_scratch(fs);
    virtio_fs_ktype_release(&fs->kobj); assert(!live_allocations);
}
static void test_capacity_reuse_and_argument_lifetime(void) {
    reset(); struct virtio_fs *fs = make_fs(2);
    assert(virtio_fs_alloc_sg_scratch(fs) == 0);
    observed_vq = &fs->vqs[1];
    struct scatterlist *scratch = observed_vq->sg_scratch;
    struct fixture first, second, small, too_large;
    request(&first, 256, 256, 0x11);
    assert(sg_count_fuse_req(&first.req) == VIRTIO_FS_MAX_SGS);
    assert(virtio_fs_enqueue_req(observed_vq, &first.req, false) == 0);
    assert(last_count == 516 && last_sg_base == scratch && observed_vq->in_flight == 1);
    assert(test_bit(FR_SENT, &first.req.flags) && first.req.list.entries == 1);
    void *first_argbuf = first.req.argbuf;
    request(&second, 256, 0, 0x22);
    assert(virtio_fs_enqueue_req(observed_vq, &second.req, false) == 0);
    assert(last_count == 260 && last_sg_base == scratch && observed_vq->in_flight == 2);
    assert(second.req.argbuf != first_argbuf && !memcmp(first_argbuf, first.input, 16));
    assert(!memcmp(second.req.argbuf, second.input, 16) && scratch_attempts == 2);
    assert(argument_allocations == 2 && notifications == 2 && processing.entries == 2);
    complete(&first, 0x61); complete(&second, 0x62);
    request(&small, 0, 0, 0x33);
    assert(virtio_fs_enqueue_req(observed_vq, &small.req, true) == 0);
    assert(last_count == 4 && last_sg_base != scratch && observed_vq->in_flight == 2);
    complete(&small, 0x63);
    unsigned attempts = argument_allocations, count = observed_vq->in_flight;
    request(&too_large, 257, 257, 0x44);
    assert(virtio_fs_enqueue_req(observed_vq, &too_large.req, false) == -EIO);
    assert(warnings == 1 && argument_allocations == attempts && !too_large.req.argbuf);
    assert(observed_vq->in_flight == count && !test_bit(FR_SENT, &too_large.req.flags));
    virtio_fs_ktype_release(&fs->kobj); assert(!live_allocations);
}
static void test_submission_errors_preserve_counts_and_scratch(void) {
    reset(); struct virtio_fs *fs = make_fs(2);
    assert(virtio_fs_alloc_sg_scratch(fs) == 0); observed_vq = &fs->vqs[1];
    struct scatterlist *scratch = observed_vq->sg_scratch;
    struct fixture item;
    request(&item, 256, 0, 0x55); add_error = -ENOSPC;
    assert(virtio_fs_enqueue_req(observed_vq, &item.req, false) == -ENOSPC);
    assert(!item.req.argbuf && !observed_vq->in_flight && !processing.entries);
    assert(!test_bit(FR_SENT, &item.req.flags) && observed_vq->sg_scratch == scratch && !scratch_frees);
    observed_vq->in_flight = 1; add_error = 0; kick_notify = false;
    assert(virtio_fs_enqueue_req(observed_vq, &item.req, true) == 0);
    assert(observed_vq->in_flight == 1 && !notifications); complete(&item, 0x66);
    request(&item, 256, 0, 0x77); observed_vq->connected = false;
    unsigned before = adds;
    assert(virtio_fs_enqueue_req(observed_vq, &item.req, true) == -ENOTCONN);
    assert(adds == before && !item.req.argbuf && observed_vq->in_flight == 1 && !locks);
    observed_vq->connected = true; fail_argument = true;
    assert(virtio_fs_enqueue_req(observed_vq, &item.req, false) == -ENOMEM);
    assert(adds == before && !item.req.argbuf && observed_vq->in_flight == 1 && !locks);
    virtio_fs_ktype_release(&fs->kobj); assert(!live_allocations);
}
int main(void) {
    test_partial_allocation_and_final_owner();
    test_capacity_reuse_and_argument_lifetime();
    test_submission_errors_preserve_counts_and_scratch();
    return 0;
}
"""


if __name__ == "__main__":
    unittest.main()
