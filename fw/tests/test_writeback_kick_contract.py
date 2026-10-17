"""Execute 0049's actual queue body with 0048's retained pressure/lifecycle code.

The unchanged 0048 test remains a baseline contract. Observable scheduler and
counter stubs here additionally reject exact sums under queue admission,
redundant queue throttling and empty submit kicks. Real batching and concurrent
writeback progress still require the separately registered guest experiments.
"""

import re
import unittest
from pathlib import Path

import test_local_flush_contract as extract
import test_writeback_pipeline_contract as baseline


PATCH = (Path(__file__).resolve().parents[1] / 'patches' /
         '0049-fuse-coalesce-writeback-kicks.patch')


def candidate():
    lines, hunk = [], False
    for line in PATCH.read_text().splitlines():
        if line.startswith('@@'):
            hunk = True
        elif hunk and line.startswith((' ', '+')):
            lines.append(line[1:])
    return '\n'.join(lines)


class WritebackKickContractTests(unittest.TestCase):
    def test_actual_queue_batch_hint_cost_and_inflight_scheduling(self):
        old = baseline.candidate_section('fs/fuse/file.c')
        old = old[old.index('/* The queue holds existing cache pages'):]
        new = candidate()
        signatures = [
            'static int fuse_pvisor_wb_budget_set(',
            'static unsigned long fuse_pvisor_wb_limit_pages(',
            'static unsigned long fuse_pvisor_wb_batch_pages(',
            'static bool fuse_pvisor_wb_enabled(',
            'static void fuse_pvisor_wb_error(',
            'static bool fuse_pvisor_wb_pressure(',
            'static void fuse_pvisor_wb_throttle(',
            'static bool fuse_pvisor_wb_queue(',
            'static int fuse_pvisor_wb_flush(',
            'static void fuse_pvisor_wb_complete(',
            'static void fuse_pvisor_wb_work(',
            'int fuse_pvisor_wb_enable(',
            'int fuse_pvisor_wb_drain(',
            'static int fuse_write_end(',
        ]
        changed = {'static bool fuse_pvisor_wb_queue(',
                   'static unsigned long fuse_pvisor_wb_batch_pages('}
        actual = '\n\n'.join(extract.function(new if signature in changed else old, signature)
                             for signature in signatures)
        definitions = {}
        for source in (old, new):
            for line in re.findall(r'^#define PVISOR_WB_.*$', source, re.M):
                definitions[line.split()[1]] = line
        prelude = baseline.PRELUDE.replace(
            'static s64 wb_stat(struct bdi_writeback *wb, int item) { return wb->approximate[item]; }',
            'static s64 wb_stat(struct bdi_writeback *wb, int item) { assert(!connection.pvisor_wb_lock); return wb->approximate[item]; }')
        prelude = prelude.replace(
            'static bool mod_delayed_work(struct workqueue_struct *queue, struct delayed_work *work, unsigned int delay) { return queue_work(queue, &work->work); }',
            'static unsigned int kick_now, kick_later, last_delay;\n'
            'static bool mod_delayed_work(struct workqueue_struct *queue, struct delayed_work *work, unsigned int delay) { kick_now++; last_delay = delay; return queue_work(queue, &work->work); }')
        prelude = prelude.replace(
            'static bool queue_delayed_work(struct workqueue_struct *queue, struct delayed_work *work, unsigned int delay) { return queue_work(queue, &work->work); }',
            'static bool queue_delayed_work(struct workqueue_struct *queue, struct delayed_work *work, unsigned int delay) { kick_later++; if (work->work.pending) return false; last_delay = delay; return queue_work(queue, &work->work); }')
        # Reuse only the observable baseline fixture/contracts, never its queue
        # implementation. The 0049 body above supplies that implementation.
        cases = baseline.CASES.replace('int main(void) {', 'static int baseline_contract_cases(void) {')
        baseline.compile_and_run(self, prelude + '\n'.join(definitions.values()) + '\n' + actual + cases + CASES)


CASES = r"""
static void test_batch_hint_and_no_duplicate_pressure(void) {
    assert(fuse_pvisor_wb_batch_pages(0) == (1UL << 20) / PAGE_SIZE);
    assert(fuse_pvisor_wb_batch_pages((16UL << 20) / PAGE_SIZE) == (4UL << 20) / PAGE_SIZE);
    assert(fuse_pvisor_wb_batch_pages((32UL << 20) / PAGE_SIZE) == (8UL << 20) / PAGE_SIZE);
    assert(fuse_pvisor_wb_batch_pages((64UL << 20) / PAGE_SIZE) == (16UL << 20) / PAGE_SIZE);
    assert(fuse_pvisor_wb_batch_pages((256UL << 20) / PAGE_SIZE) == (16UL << 20) / PAGE_SIZE);
    enabled(); kick_now = kick_later = 0;
    unsigned long threshold = fuse_pvisor_wb_batch_pages(connection.pvisor_wb_pages);
    bdi.wb.approximate[WB_RECLAIMABLE] = threshold - 1;
    bdi.wb.exact[WB_RECLAIMABLE] = threshold + 1;
    assert(fuse_pvisor_wb_queue(&inodes[0], &handles[0], true, false, 0));
    assert(kick_now == 0 && kick_later == 1 && last_delay == 10 && sum_calls == 0);
    assert(fuse_pvisor_wb_queue(&inodes[0], &handles[0], true, false, 0));
    assert(last_delay == 10 && kick_now == 0 && sum_calls == 0); /* Timer not extended. */
    bdi.wb.approximate[WB_RECLAIMABLE] = threshold;
    assert(fuse_pvisor_wb_queue(&inodes[0], &handles[0], true, false, 0));
    assert(kick_now == 1 && last_delay == 0 && sum_calls == 0);
    /* Queue admission is not the dirty-memory enforcement point. */
    bdi.wb.approximate[WB_RECLAIMABLE] = connection.pvisor_wb_pages;
    bdi.wb.exact[WB_RECLAIMABLE] = connection.pvisor_wb_pages;
    assert(fuse_pvisor_wb_queue(&inodes[0], &handles[0], true, false, 0));
    assert(waits == 0 && wakes == 0 && sum_calls == 0);
    memset(&bdi.wb, 0, sizeof(bdi.wb));
    assert(fuse_pvisor_wb_drain(&connection) == 0 && connection.pvisor_wb_count == 0);
}
static void test_no_empty_submit_kicks_and_completion_rescan(void) {
    enabled(); kick_now = kick_later = 0;
    assert(fuse_pvisor_wb_queue(&inodes[0], &handles[0], true, false, 0));
    assert(flush_delayed_work(&connection.pvisor_wb_work));
    assert(list_empty(&connection.pvisor_wb_pending) && submissions == 1 && completions == 0);
    unsigned int now = kick_now, later = kick_later;
    for (int i = 0; i < 100; i++)
        assert(fuse_pvisor_wb_queue(&inodes[0], &handles[0], true, false, 0));
    assert(fuse_pvisor_wb_queue(&inodes[0], &handles[0], false, true, 123));
    assert(kick_now == now && kick_later == later && !connection.pvisor_wb_work.work.pending);
    assert(connection.pvisor_wb_complete_work.pending && connection.pvisor_wb_count == 1);
    flush_workqueue(connection.pvisor_wb_complete_wq);
    assert(kick_now == now + 1 && connection.pvisor_wb_work.work.pending);
    assert(!list_empty(&connection.pvisor_wb_pending) && releases == 0);
    assert(fuse_pvisor_wb_drain(&connection) == 0);
    assert(submissions == 2 && completions == 2 && releases == 1);
    assert(connection.pvisor_wb_count == 0 && file_puts == 1 && inode_puts == 1);
}
int main(void) {
    assert(baseline_contract_cases() == 0);
    test_batch_hint_and_no_duplicate_pressure();
    test_no_empty_submit_kicks_and_completion_rescan();
    return 0;
}
"""


if __name__ == '__main__':
    unittest.main()
