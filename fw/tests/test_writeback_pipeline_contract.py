"""Execute 0048's actual queue/pressure functions against observable stubs.

The scheduler stubs let submission run while completion remains pending, and
exercise rescan, drain, sticky errors and reference/RELEASE ordering. They do
not establish real kernel concurrency, reclaim safety or performance; those
remain firmware build and guest integration checks.
"""

import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

import test_local_flush_contract as local_flush


PATCH = (Path(__file__).resolve().parents[1] / "patches" /
         "0048-fuse-pipeline-native-writeback-pressure.patch")


def candidate_section(relative):
    text = PATCH.read_text()
    start = text.index(f"diff --git a/{relative} b/{relative}\n")
    end = text.find("\ndiff --git ", start + 1)
    section = text[start:end if end >= 0 else len(text)]
    lines = []
    in_hunk = False
    for line in section.splitlines():
        if line.startswith("@@"):
            in_hunk = True
        elif in_hunk and line.startswith((" ", "+")):
            lines.append(line[1:])
    return "\n".join(lines)


def compile_and_run(test, source):
    compiler = shutil.which("cc")
    if compiler is None:
        test.skipTest("C compiler required for actual-function contract harness")
    with tempfile.TemporaryDirectory() as directory:
        directory = Path(directory)
        input_file = directory / "writeback-contract.c"
        executable = directory / "writeback-contract"
        input_file.write_text(source)
        subprocess.run([
            compiler, "-std=gnu11", "-Wall", "-Wextra", "-Werror",
            "-Wno-unused-parameter", str(input_file), "-o", str(executable),
        ], check=True)
        subprocess.run([str(executable)], check=True)


class WritebackPipelineContractTests(unittest.TestCase):
    def test_actual_pressure_pipeline_rescan_drain_and_release(self):
        source = candidate_section("fs/fuse/file.c")
        signatures = [
            "static int fuse_pvisor_wb_budget_set(",
            "static unsigned long fuse_pvisor_wb_limit_pages(",
            "static bool fuse_pvisor_wb_enabled(",
            "static void fuse_pvisor_wb_error(",
            "static bool fuse_pvisor_wb_pressure(",
            "static void fuse_pvisor_wb_throttle(",
            "static bool fuse_pvisor_wb_queue(",
            "static int fuse_pvisor_wb_flush(",
            "static void fuse_pvisor_wb_complete(",
            "static void fuse_pvisor_wb_work(",
            "int fuse_pvisor_wb_enable(",
            "int fuse_pvisor_wb_drain(",
            "static int fuse_write_end(",
        ]
        # The forward declarations are deliberately excluded from extraction.
        source = source[source.index("/* The queue holds existing cache pages") :]
        actual = "\n\n".join(local_flush.function(source, item) for item in signatures)
        definitions = "\n".join(re.findall(r"^#define PVISOR_WB_.*$", source, re.M))
        compile_and_run(self, PRELUDE + definitions + "\n" + actual + CASES)

    def test_readonly_close_keeps_native_flush_and_error_checks(self):
        source = candidate_section("fs/fuse/file.c")
        actual = "\n".join([
            local_flush.function(source, "static bool fuse_pvisor_local_flush_enabled("),
            local_flush.function(source, "static int fuse_flush("),
        ])
        header = local_flush.candidate_section("include/uapi/linux/fuse.h")
        definitions = "\n".join(re.findall(
            r"^#define F(?:OPEN|USE)_PVISOR_LOCAL_FLUSH\s+.*$", header, re.M
        ))
        prelude = local_flush.PRELUDE.replace(
            "static bool fuse_pvisor_wb_enabled(struct inode *inode, struct fuse_file *ff) { return false; }",
            "static bool fuse_pvisor_wb_enabled(struct inode *inode, struct fuse_file *ff) { return inode->mount->fc->writeback_cache; }",
        )
        cases = local_flush.CASES.replace("    return 0;\n}", r"""
    reset(); connection.writeback_cache = true; rpc_error = -EIO;
    expect(-EIO, "WLSUER"); /* WB does not authorize local readonly FLUSH. */
    reset(); connection.writeback_cache = true; mapping_error = -ENOSPC;
    expect(-ENOSPC, "WLSUE");
    reset(); connection.writeback_cache = true; file.f_mode = FMODE_WRITE;
    expect(0, "");
    reset(); connection.writeback_cache = true; file.f_mode = FMODE_WRITE;
    connection.pvisor_wb_error = -EIO; expect(-EIO, "");
    return 0;
}""")
        compile_and_run(self, prelude + definitions + "\n" + actual + cases)


PRELUDE = r"""
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <sys/stat.h>

typedef uint64_t u64;
typedef int64_t s64;
typedef long long kernel_offset_t;
struct kernel_param { int unused; };
#define loff_t kernel_offset_t
#define PAGE_SHIFT 12
#define PAGE_SIZE (1UL << PAGE_SHIFT)
#define PAGE_MASK (~(PAGE_SIZE - 1))
#define WB_RECLAIMABLE 0
#define WB_WRITEBACK 1
#define WB_REASON_BACKGROUND 0
#define WQ_UNBOUND 1
#define WQ_MEM_RECLAIM 2
#define FOPEN_DIRECT_IO (1U << 0)
#define FUSE_FLUSH 25
#define FUSE_IS_DAX(inode) ((inode)->dax)
#define READ_ONCE(value) (value)
#define WRITE_ONCE(variable, value) ((variable) = (value))
#define clamp_t(type, value, low, high) ((type)(value) < (type)(low) ? (type)(low) : ((type)(value) > (type)(high) ? (type)(high) : (type)(value)))
#define msecs_to_jiffies(value) (value)
#define container_of(pointer, type, member) ((type *)((char *)(pointer) - offsetof(type, member)))
#define pr_err(...) ((void)0)
#define pr_warn(...) (++warnings)
#define FUSE_ARGS(name) struct fuse_args name = {0}

struct list_head { struct list_head *next, *prev; };
#define LIST_HEAD(name) struct list_head name = { &(name), &(name) }
static void INIT_LIST_HEAD(struct list_head *head) { head->next = head->prev = head; }
static bool list_empty(struct list_head *head) { return head->next == head; }
static void list_add_tail(struct list_head *item, struct list_head *head) {
    item->next = head; item->prev = head->prev;
    head->prev->next = item; head->prev = item;
}
static void list_del_init(struct list_head *item) {
    item->prev->next = item->next; item->next->prev = item->prev;
    INIT_LIST_HEAD(item);
}
static void list_splice_init(struct list_head *from, struct list_head *to) {
    if (list_empty(from)) return;
    from->next->prev = to; from->prev->next = to->next;
    to->next->prev = from->prev; to->next = from->next;
    INIT_LIST_HEAD(from);
}
static void list_splice_tail_init(struct list_head *from, struct list_head *to) {
    if (list_empty(from)) return;
    from->next->prev = to->prev; to->prev->next = from->next;
    from->prev->next = to; to->prev = from->prev;
    INIT_LIST_HEAD(from);
}
#define list_entry(pointer, type, member) container_of(pointer, type, member)
#define list_for_each_entry(item, head, member) \
    for (item = list_entry((head)->next, __typeof__(*item), member); \
         &(item)->member != (head); \
         item = list_entry((item)->member.next, __typeof__(*item), member))
#define list_for_each_entry_safe(item, next_item, head, member) \
    for (item = list_entry((head)->next, __typeof__(*item), member), \
         next_item = list_entry((item)->member.next, __typeof__(*item), member); \
         &(item)->member != (head); item = next_item, \
         next_item = list_entry((next_item)->member.next, __typeof__(*next_item), member))

struct work_struct { void (*function)(struct work_struct *); bool pending; };
struct delayed_work { struct work_struct work; };
struct workqueue_struct { struct work_struct *work; bool destroyed; };
#define to_delayed_work(pointer) container_of(pointer, struct delayed_work, work)
#define INIT_WORK(work, function_value) (*(work) = (struct work_struct){ .function = function_value })
#define INIT_DELAYED_WORK(delayed, function_value) INIT_WORK(&(delayed)->work, function_value)
struct bdi_writeback { s64 approximate[2], exact[2]; };
struct backing_dev_info { struct bdi_writeback wb; };
struct super_block { struct backing_dev_info *s_bdi; };
struct fuse_conn {
    struct workqueue_struct *pvisor_wb_wq, *pvisor_wb_complete_wq;
    struct delayed_work pvisor_wb_work;
    struct work_struct pvisor_wb_complete_work;
    int pvisor_wb_lock, pvisor_wb_wait;
    struct list_head pvisor_wb_pending, pvisor_wb_completing;
    unsigned long pvisor_wb_pages;
    unsigned int pvisor_wb_count;
    int pvisor_wb_error;
    bool connected, no_flush, auto_submounts;
};
struct fuse_mount { struct fuse_conn *fc; struct super_block *sb; };
struct address_space { struct inode *host; };
struct inode {
    struct fuse_mount *mount;
    struct address_space *i_mapping;
    unsigned int i_mode;
    loff_t i_size;
    bool dax, locked, temporary_write;
    int references;
};
struct fuse_file {
    struct fuse_mount *fm;
    struct list_head pvisor_wb_entry;
    struct inode *pvisor_wb_inode;
    unsigned int open_flags, pvisor_wb_flags;
    bool pvisor_wb_queued, pvisor_wb_released, pvisor_wb_redirtied;
    u64 fh;
    int references;
};
struct file { struct fuse_file *private_data; };
struct folio { struct address_space *mapping; bool locked, put, uptodate; };
struct fuse_flush_in { u64 fh; };
struct fuse_args {
    unsigned int opcode, nodeid, in_numargs;
    struct { unsigned int size; const void *value; } in_args[1];
    bool force, nocreds;
};

static struct fuse_conn connection;
static struct backing_dev_info bdi;
static struct super_block super;
static struct fuse_mount mount;
static struct workqueue_struct workqueues[2];
static struct inode inodes[2];
static struct address_space mappings[2];
static struct fuse_file handles[2];
static struct folio *observed_folio;
static int allocations, fail_allocation, warnings, reservation_error;
static int min_calls, max_calls, sum_calls, wakes, waits, submissions, completions;
static int rpc_error, write_error, submit_error, mapping_error, progress_mode;
static int releases, file_puts, inode_puts;
static u64 reserved_bytes;
static unsigned long pvisor_wb_budget_bytes;
static char events[1024];
static size_t event_count;
static void event(char value) { assert(event_count + 1 < sizeof(events)); events[event_count++] = value; events[event_count] = '\0'; }
static struct fuse_conn *get_fuse_conn(struct inode *inode) { return inode->mount->fc; }
static struct backing_dev_info *inode_to_bdi(struct inode *inode) { return inode->mount->sb->s_bdi; }
static s64 wb_stat(struct bdi_writeback *wb, int item) { return wb->approximate[item]; }
static s64 wb_stat_sum(struct bdi_writeback *wb, int item) { sum_calls++; return wb->exact[item]; }
static unsigned long wb_stat_error(void) { return 64; }
static void spin_lock(int *lock) { assert(!*lock); *lock = 1; }
static void spin_unlock(int *lock) { assert(*lock); *lock = 0; }
static int cmpxchg(int *address, int old, int value) { int before = *address; if (before == old) *address = value; return before; }
static void ihold(struct inode *inode) { inode->references++; }
static void fuse_file_get(struct fuse_file *ff) { ff->references++; }
static void wake_up_all(int *wait) { }
static bool queue_work(struct workqueue_struct *queue, struct work_struct *work) { queue->work = work; bool fresh = !work->pending; work->pending = true; return fresh; }
static bool mod_delayed_work(struct workqueue_struct *queue, struct delayed_work *work, unsigned int delay) { return queue_work(queue, &work->work); }
static bool queue_delayed_work(struct workqueue_struct *queue, struct delayed_work *work, unsigned int delay) { return queue_work(queue, &work->work); }
static bool flush_delayed_work(struct delayed_work *work) {
    bool pending = work->work.pending; work->work.pending = false;
    if (pending) work->work.function(&work->work);
    return pending;
}
static void flush_workqueue(struct workqueue_struct *queue) {
    int guard = 0;
    while (queue->work && queue->work->pending) {
        assert(++guard < 20); queue->work->pending = false;
        queue->work->function(queue->work);
    }
}
static void wait_progress(void) {
    waits++;
    if (progress_mode == 1) connection.pvisor_wb_error = -EIO;
    else if (progress_mode == 2) connection.connected = false;
    else memset(&bdi.wb, 0, sizeof(bdi.wb));
}
#define wait_event_timeout(wait, condition, timeout) \
    do { if (!(condition)) wait_progress(); assert(condition); } while (0)
#define wait_event(wait, condition) \
    do { int guard = 0; while (!(condition)) { assert(++guard < 20); \
         flush_delayed_work(&connection.pvisor_wb_work); \
         flush_workqueue(connection.pvisor_wb_complete_wq); } } while (0)
static void wakeup_flusher_threads_bdi(struct backing_dev_info *target, int reason) {
    assert(target == &bdi && !connection.pvisor_wb_lock);
    if (observed_folio) assert(!observed_folio->locked && observed_folio->put);
    wakes++;
}
static int filemap_flush(struct address_space *mapping) { submissions++; mapping->host->temporary_write = true; event('F'); return submit_error; }
static int write_inode_now(struct inode *inode, int sync) {
    assert(sync == 1 && !inode->locked);
    /* Sending SETATTR before the previously submitted WRITE reply is the bug. */
    assert(!inode->temporary_write);
    /* This native integrity pass may itself submit previously skipped pages. */
    inode->temporary_write = true; completions++; event('W'); return write_error;
}
static void inode_lock(struct inode *inode) { assert(!inode->locked); inode->locked = true; event('L'); }
static void fuse_sync_writes(struct inode *inode) { assert(inode->locked); inode->temporary_write = false; event('S'); }
static void inode_unlock(struct inode *inode) { assert(inode->locked); inode->locked = false; event('U'); }
static int filemap_check_errors(struct address_space *mapping) { assert(!mapping->host->temporary_write); event('E'); return mapping_error; }
static unsigned int get_node_id(struct inode *inode) { return 42; }
static int fuse_simple_request(struct fuse_mount *fm, struct fuse_args *args) {
    assert(args->opcode == FUSE_FLUSH && args->force && args->nocreds);
    assert(args->nodeid == 42 && connection.pvisor_wb_count > 0);
    event('R'); return rpc_error;
}
static void fuse_file_release(struct inode *inode, struct fuse_file *ff, unsigned int flags, u64 owner, bool directory) {
    assert(flags == 123 && !directory && connection.pvisor_wb_count > 0);
    assert(ff->references == 2 && inode->references == 2);
    assert(!inode->temporary_write && !inode->locked);
    releases++; event('r');
}
static void fuse_file_put(struct fuse_file *ff, bool sync) {
    assert(sync == connection.auto_submounts && connection.pvisor_wb_count > 0);
    assert(ff->references > 1); ff->references--; file_puts++; event('P');
}
static void iput(struct inode *inode) {
    assert(connection.pvisor_wb_count > 0 && inode->references > 1);
    inode->references--; inode_puts++; event('I');
}
static unsigned long totalram_pages(void) { return (512UL << 20) >> PAGE_SHIFT; }
static struct workqueue_struct *alloc_workqueue(const char *name, unsigned int flags, int active) {
    assert(active == 1 && flags == (WQ_UNBOUND | WQ_MEM_RECLAIM));
    allocations++;
    return allocations == fail_allocation ? NULL : &workqueues[allocations - 1];
}
static void destroy_workqueue(struct workqueue_struct *queue) { queue->destroyed = true; }
static int bdi_set_max_ratio(struct backing_dev_info *target, unsigned int ratio) { assert(ratio == 100); max_calls++; return reservation_error; }
static int bdi_set_min_bytes(struct backing_dev_info *target, u64 bytes) { min_calls++; reserved_bytes = bytes; return reservation_error; }
static bool folio_test_uptodate(struct folio *folio) { return folio->uptodate; }
static void folio_zero_segment(struct folio *folio, size_t start, size_t end) { assert(start <= end); }
static void folio_mark_uptodate(struct folio *folio) { folio->uptodate = true; }
static void i_size_write(struct inode *inode, loff_t size) { inode->i_size = size; }
static void folio_mark_dirty(struct folio *folio) { bdi.wb.approximate[WB_RECLAIMABLE]++; bdi.wb.exact[WB_RECLAIMABLE]++; }
static void folio_unlock(struct folio *folio) { assert(folio->locked); folio->locked = false; }
static void folio_put(struct folio *folio) { folio->put = true; }
"""


CASES = r"""
static void reset(void) {
    memset(&connection, 0, sizeof(connection));
    memset(&bdi, 0, sizeof(bdi));
    memset(workqueues, 0, sizeof(workqueues));
    memset(handles, 0, sizeof(handles));
    memset(inodes, 0, sizeof(inodes));
    INIT_LIST_HEAD(&connection.pvisor_wb_pending);
    INIT_LIST_HEAD(&connection.pvisor_wb_completing);
    connection.connected = true; connection.auto_submounts = true;
    super.s_bdi = &bdi; mount.fc = &connection; mount.sb = &super;
    for (int i = 0; i < 2; i++) {
        mappings[i].host = &inodes[i];
        inodes[i].mount = &mount; inodes[i].i_mapping = &mappings[i];
        inodes[i].i_mode = 0100000 | 0644; inodes[i].references = 1;
        handles[i].fm = &mount; handles[i].references = 1;
        INIT_LIST_HEAD(&handles[i].pvisor_wb_entry);
    }
    allocations = fail_allocation = warnings = reservation_error = 0;
    min_calls = max_calls = sum_calls = wakes = waits = 0;
    submissions = completions = rpc_error = write_error = submit_error = 0;
    mapping_error = progress_mode = releases = file_puts = inode_puts = 0;
    observed_folio = NULL; reserved_bytes = 0; event_count = 0; events[0] = '\0';
}
static void enabled(void) { reset(); assert(fuse_pvisor_wb_enable(&mount) == 0); }
static void test_budget_and_admission(void) {
    assert(fuse_pvisor_wb_budget_set("0", NULL) == -EPERM);
    assert(fuse_pvisor_wb_budget_set("9999999999999999", NULL) == -EPERM);
    assert(fuse_pvisor_wb_limit_pages(1) == (16UL << 20) / PAGE_SIZE);
    assert(fuse_pvisor_wb_limit_pages((512UL << 20) / PAGE_SIZE) == (64UL << 20) / PAGE_SIZE);
    assert(fuse_pvisor_wb_limit_pages((16UL << 30) / PAGE_SIZE) == (256UL << 20) / PAGE_SIZE);
    enabled(); assert(reserved_bytes == (16UL << 20));
    assert(pvisor_wb_budget_bytes == (64UL << 20));
    assert(max_calls == 1 && min_calls == 1 && warnings == 0);
    assert(connection.pvisor_wb_wq != connection.pvisor_wb_complete_wq);
    reset(); fail_allocation = 1; assert(fuse_pvisor_wb_enable(&mount) == -ENOMEM);
    assert(!connection.pvisor_wb_wq && !connection.pvisor_wb_complete_wq);
    reset(); fail_allocation = 2; assert(fuse_pvisor_wb_enable(&mount) == -ENOMEM);
    assert(workqueues[0].destroyed && !connection.pvisor_wb_complete_wq);
    reset(); reservation_error = -EINVAL; assert(fuse_pvisor_wb_enable(&mount) == 0);
    assert(warnings == 1 && max_calls == 1 && min_calls == 0);
}
static void test_actual_pages_and_pressure_exit(void) {
    enabled(); unsigned long limit = connection.pvisor_wb_pages;
    assert(!fuse_pvisor_wb_pressure(&inodes[0]) && sum_calls == 0);
    bdi.wb.approximate[WB_RECLAIMABLE] = limit - 128;
    bdi.wb.exact[WB_RECLAIMABLE] = limit - 1;
    assert(!fuse_pvisor_wb_pressure(&inodes[0]) && sum_calls == 2);
    bdi.wb.exact[WB_WRITEBACK] = 1;
    assert(fuse_pvisor_wb_pressure(&inodes[0]));
    fuse_pvisor_wb_throttle(&inodes[0]); assert(wakes == 1 && waits == 1);
    assert(!fuse_pvisor_wb_pressure(&inodes[0]));
    bdi.wb.approximate[WB_WRITEBACK] = bdi.wb.exact[WB_WRITEBACK] = limit;
    progress_mode = 1; fuse_pvisor_wb_throttle(&inodes[0]);
    assert(connection.pvisor_wb_error == -EIO && waits == 2);
    connection.pvisor_wb_error = 0; progress_mode = 2;
    fuse_pvisor_wb_throttle(&inodes[0]); assert(!connection.connected && waits == 3);
}
static void test_pipeline_overwrites_rescan_and_release(void) {
    enabled();
    for (int i = 0; i < 100; i++) assert(fuse_pvisor_wb_queue(&inodes[0], &handles[0], true, false, 0));
    assert(connection.pvisor_wb_count == 1 && handles[0].references == 2);
    assert(bdi.wb.exact[WB_RECLAIMABLE] == 0); /* Queue has no byte ledger. */
    flush_delayed_work(&connection.pvisor_wb_work);
    assert(submissions == 1 && completions == 0);
    assert(fuse_pvisor_wb_queue(&inodes[1], &handles[1], true, false, 0));
    flush_delayed_work(&connection.pvisor_wb_work);
    assert(submissions == 2 && completions == 0 && !strcmp(events, "FF"));
    /* First handle is modified and closed while its completion is pending. */
    assert(fuse_pvisor_wb_queue(&inodes[0], &handles[0], true, true, 123));
    flush_workqueue(connection.pvisor_wb_complete_wq);
    assert(connection.pvisor_wb_count == 1 && releases == 0);
    assert(handles[0].pvisor_wb_queued && handles[0].references == 2);
    assert(inodes[1].references == 1 && handles[1].references == 1);
    assert(fuse_pvisor_wb_drain(&connection) == 0);
    assert(submissions == 3 && completions == 3 && releases == 1);
    assert(connection.pvisor_wb_count == 0 && file_puts == 2 && inode_puts == 2);
    assert(!handles[0].pvisor_wb_queued && handles[0].references == 1);
    assert(strstr(events, "LSUWLSUERrPI") != NULL); /* Old WRITEs, integrity, new WRITEs, FLUSH, RELEASE, refs. */
}
static void test_sticky_error_does_not_abandon_release(void) {
    enabled(); submit_error = -ENOSPC; write_error = -EIO; rpc_error = -EBADF;
    assert(fuse_pvisor_wb_queue(&inodes[0], &handles[0], true, true, 123));
    assert(fuse_pvisor_wb_drain(&connection) == -ENOSPC);
    assert(connection.pvisor_wb_count == 0 && releases == 1);
    assert(file_puts == 1 && inode_puts == 1);
    fuse_pvisor_wb_error(&connection, 0);
    assert(fuse_pvisor_wb_drain(&connection) == -ENOSPC);
    enabled(); rpc_error = -ENOSYS;
    assert(fuse_pvisor_wb_queue(&inodes[0], &handles[0], false, true, 123));
    assert(fuse_pvisor_wb_drain(&connection) == 0 && connection.no_flush);
}
static void test_long_write_pressure_after_folio_unlock(void) {
    enabled();
    struct file file = { .private_data = &handles[0] };
    struct folio folio = { .mapping = &mappings[0], .locked = true, .uptodate = true };
    observed_folio = &folio;
    bdi.wb.approximate[WB_RECLAIMABLE] = connection.pvisor_wb_pages - 1;
    bdi.wb.exact[WB_RECLAIMABLE] = connection.pvisor_wb_pages - 1;
    assert(fuse_write_end(&file, &mappings[0], 0, PAGE_SIZE, PAGE_SIZE, &folio, NULL) == PAGE_SIZE);
    assert(wakes == 1 && waits == 1 && inodes[0].i_size == PAGE_SIZE);
    assert(connection.pvisor_wb_count == 0); /* No queue-capacity wait under i_rwsem. */
    folio.locked = true; folio.put = false;
    assert(fuse_write_end(&file, &mappings[0], PAGE_SIZE, PAGE_SIZE, 0, &folio, NULL) == 0);
    assert(wakes == 1 && waits == 1 && folio.put && !folio.locked);
}
int main(void) {
    test_budget_and_admission();
    test_actual_pages_and_pressure_exit();
    test_pipeline_overwrites_rescan_and_release();
    test_sticky_error_does_not_abandon_release();
    test_long_write_pressure_after_folio_unlock();
    return 0;
}
"""


if __name__ == "__main__":
    unittest.main()
