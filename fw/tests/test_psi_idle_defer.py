"""Execute the added kernel C with an on-time aggregation oracle.

Minimal scheduler/timer stubs do not model kernel concurrency. Real VM tests
cover wake/trigger interaction. The averaging oracle is pinned to Linux 6.12.109.
"""
from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import unittest


def function(text, name):
    start = text.index(name + '(')
    start = text.rfind('\n', 0, start) + 1
    end = text.index('\n}', start) + 2
    return text[start:end]


class PsiDeferredTests(unittest.TestCase):
    def test_timed_checkpoint_and_conservative_admission(self):
        cc = shutil.which('cc')
        if not cc:
            self.skipTest('C compiler required')
        patch = (Path(__file__).parents[1] / 'patches/0053-psi-idle-defer.patch').read_text()
        added = '\n'.join(l[1:] for l in patch.splitlines()
                          if l.startswith('+') and not l.startswith('+++'))
        record = added[added.index('static struct psi_idle_record {'):added.index('} psi_deferred;') + len('} psi_deferred;')]
        source = (Path(__file__).parent / 'psi_defer_test_stubs.h').read_text()
        if os.environ.get('PVISOR_TEST_PSI_SOURCE'):
            kernel = Path(os.environ['PVISOR_TEST_PSI_SOURCE']).read_text()
            # Compile the real pinned averaging function, not just its oracle.
            source = source.replace(function(source, 'update_averages'), function(kernel, 'update_averages'))
        source += '\n' + record + '\n'
        for name in ['psi_defer_enabled', 'psi_defer_wake', 'psi_defer_flush', 'psi_pvisor_idle_defer']:
            source += function(added, name) + '\n'
        source += r'''
static void reset(void)
{
    memset(&psi_system, 0, sizeof(psi_system));
    memset(&cpu, 0, sizeof(cpu));
    memset(&psi_deferred, 0, sizeof(psi_deferred));
    psi_system.pcpu = &cpu;
    psi_system.avg_last_update = 1000000000ULL;
    psi_system.avg_next_update = 1000000000ULL + psi_period;
    psi_system.avgs_work.timer.pending = true;
    psi_system.avgs_work.timer.expires = 751;
    psi_system.avgs_work.pending = true;
    jiffies = 500; clock_ns = 2000000000ULL;
    psi_idle_sample_ns = 1000000000ULL;
    cpu.times[PSI_CPU_SOME] = 100000000;
    cpu.times[PSI_NONIDLE] = 500000000;
    psi_defer_trigger_inflight = 0; possible = 1; irq_account = false;
    psi_idle_defer = psi_idle_quiesce = true; psi_disabled = false;
}
static void rejected(void)
{
    struct psi_group before = psi_system;
    struct psi_group_cpu c = cpu;
    psi_pvisor_idle_defer();
    assert(!psi_deferred.valid);
    assert(!memcmp(&before, &psi_system, sizeof(before)));
    assert(!memcmp(&c, &cpu, sizeof(c)));
}
static void replay(u64 sleep_ns, u64 carry, u32 raw)
{
    reset();
    cpu.times[PSI_CPU_SOME] = raw;
    psi_system.total[PSI_AVGS][PSI_CPU_SOME] = carry;
    psi_pvisor_idle_defer();
    assert(psi_deferred.valid && !psi_system.avgs_work.pending);
    assert(psi_deferred.captured_ns == clock_ns);
    u64 due = psi_deferred.due_ns, now = due + sleep_ns;
    struct psi_group oracle = psi_system;
    /* On-time aggregation of the frozen sample, then ordinary idle catch-up. */
    oracle.total[PSI_AVGS][PSI_CPU_SOME] += raw;
    oracle.avg_next_update = update_averages(&oracle, due);
    while (oracle.avg_next_update <= now &&
           oracle.total[PSI_AVGS][PSI_CPU_SOME] != oracle.avg_total[PSI_CPU_SOME])
        oracle.avg_next_update = update_averages(&oracle, oracle.avg_next_update);
    if (now >= oracle.avg_next_update)
        oracle.avg_next_update = update_averages(&oracle, now);
    /* New wake activity must not be attributed to the old checkpoint. */
    cpu.times[PSI_CPU_SOME] += 12345;
    psi_defer_wake(&psi_system, now);
    assert(psi_deferred.valid && psi_system.avgs_work.pending);
    psi_defer_wake(&psi_system, now + 1); /* same record may only kick once */
    assert(psi_deferred.wake_queued);
    psi_defer_flush(&psi_system, now);
    assert(!psi_deferred.valid);
    assert(!memcmp(oracle.total, psi_system.total, sizeof(oracle.total)));
    assert(!memcmp(oracle.avg, psi_system.avg, sizeof(oracle.avg)));
    assert(!memcmp(oracle.avg_total, psi_system.avg_total, sizeof(oracle.avg_total)));
    assert(oracle.avg_last_update == psi_system.avg_last_update);
    assert(oracle.avg_next_update == psi_system.avg_next_update);
    assert((u32)(cpu.times[PSI_CPU_SOME] - cpu.times_prev[PSI_AVGS][PSI_CPU_SOME]) == 12345);
    assert(cpu.times_prev[PSI_POLL][PSI_CPU_SOME] == 0);
    struct psi_group once = psi_system;
    psi_defer_flush(&psi_system, now); /* No double counting. */
    assert(!memcmp(&once, &psi_system, sizeof(once)));
}
int main(void)
{
    for (unsigned sec = 0; sec <= 180; sec++) {
        replay(sec * 1000000000ULL, 0, 100000000);
        replay(sec * 1000000000ULL, 3000000000ULL, 1000000000);
    }
    reset(); psi_pvisor_idle_defer();
    psi_defer_wake(&psi_system, clock_ns + 1);
    assert(!psi_deferred.valid && psi_system.avgs_work.pending);
    assert(psi_system.avgs_work.delay == 251);
    assert(cpu.times_prev[PSI_AVGS][PSI_CPU_SOME] == 0);
    reset(); psi_pvisor_idle_defer();
    psi_defer_flush(&psi_system, clock_ns + 1); /* Early read/registration. */
    assert(!psi_deferred.valid && cpu.times_prev[PSI_AVGS][PSI_CPU_SOME] == 0);
    reset(); cpu.times_prev[PSI_AVGS][PSI_CPU_SOME] = UINT32_MAX - 99;
    cpu.times[PSI_CPU_SOME] = 100;
    psi_pvisor_idle_defer(); psi_defer_flush(&psi_system, 30000000000ULL);
    assert(psi_system.total[PSI_AVGS][PSI_CPU_SOME] == 200); /* modulo delta */
    reset(); cpu.times[PSI_NONIDLE] = 1;
    psi_pvisor_idle_defer(); psi_defer_flush(&psi_system, 30000000000ULL);
    assert(psi_system.total[PSI_AVGS][PSI_CPU_SOME] == 0); /* original weighting */
    reset(); possible = 2; rejected();
    reset(); irq_account = true; rejected();
    reset(); psi_idle_defer = false; rejected();
    reset(); psi_disabled = true; rejected();
    reset(); psi_idle_quiesce = false; rejected();
    reset(); psi_defer_trigger_inflight = 1; rejected();
    reset(); cpu.state_mask = BIT(PSI_NONIDLE); rejected();
    reset(); psi_idle_sample_ns = 0; rejected();
    reset(); clock_ns = psi_idle_sample_ns + U32_MAX; rejected();
    reset(); psi_system.avgs_work.timer.pending = false; rejected();
    reset(); psi_system.avgs_work.timer.expires = jiffies; rejected();
    reset(); psi_system.avg_next_update = 10000000000ULL; rejected();
    reset(); psi_system.total[PSI_AVGS][0] = 5 * psi_period; rejected();
    for (int s = 0; s < PSI_NONIDLE; s++) {
        reset(); psi_system.avg_nr_triggers[s] = 1; rejected();
        reset(); psi_system.rtpoll_nr_triggers[s] = 1; rejected();
    }
    puts("timed replay: 362 idle/carry cases plus early wake, wrap, bounds and trigger guards passed");
}
'''
        with tempfile.TemporaryDirectory(prefix='pvisor-psi-defer-') as td:
            p = Path(td)
            (p / 'test.c').write_text(source)
            subprocess.run([cc, '-std=c11', '-Wall', '-Wextra', '-Werror',
                            str(p / 'test.c'), '-o', str(p / 'test')], check=True)
            subprocess.run([str(p / 'test')], check=True)


if __name__ == '__main__':
    unittest.main()
