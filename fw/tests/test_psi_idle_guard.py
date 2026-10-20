"""Compile the actual added guard with minimal types; exercise conservative boundaries.

This does not model scheduler concurrency or replace real-VM PSI validation.
"""
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class PsiIdleGuardTests(unittest.TestCase):
    def test_guard_rejects_unsettled_or_out_of_scope_state(self):
        cc = shutil.which('cc')
        if not cc:
            self.skipTest('C compiler required for the extracted kernel guard')
        patch = (Path(__file__).parents[1] / 'patches/0052-psi-idle-quiesce.patch').read_text()
        added = '\n'.join(line[1:] for line in patch.splitlines()
                          if line.startswith('+') and not line.startswith('+++'))
        start = added.index('static bool psi_idle_can_quiesce(')
        end = added.index('\n}', start) + 2
        guard = added[start:end]
        source = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
typedef uint64_t u64;
typedef uint32_t u32;
#define U32_MAX UINT32_MAX
#define BIT(n) (1U << (n))
#define READ_ONCE(x) (x)
enum psi_states { PSI_IO_SOME, PSI_IO_FULL, PSI_MEM_SOME, PSI_MEM_FULL,
                  PSI_CPU_SOME, PSI_CPU_FULL, PSI_NONIDLE, NR_PSI_STATES };
enum { PSI_AVGS, PSI_POLL };
struct psi_group { u32 avg_nr_triggers[6], rtpoll_nr_triggers[6]; };
struct psi_group_cpu { u32 state_mask, times[7], times_prev[2][7]; };
static struct psi_group psi_system;
static bool psi_idle_quiesce = true;
static u64 psi_idle_sample_ns = 100;
static unsigned possible_cpus = 1;
static unsigned num_possible_cpus(void) { return possible_cpus; }
''' + guard + r'''
static void check(struct psi_group *g, struct psi_group_cpu *c, u64 now, bool expected)
{
    struct psi_group oldg = *g;
    struct psi_group_cpu oldc = *c;
    assert(psi_idle_can_quiesce(g, c, now) == expected);
    /* Admission testing must not discard or adjust the pressure counters. */
    assert(memcmp(&oldg, g, sizeof(*g)) == 0);
    assert(memcmp(&oldc, c, sizeof(*c)) == 0);
}
int main(void)
{
    struct psi_group_cpu c = {0};
    struct psi_group child = {0};
    check(&psi_system, &c, 200, true);
    psi_idle_quiesce = false; check(&psi_system, &c, 200, false);
    psi_idle_quiesce = true;
    possible_cpus = 2; check(&psi_system, &c, 200, false); possible_cpus = 1;
    check(&child, &c, 200, false);
    c.state_mask = BIT(PSI_NONIDLE); check(&psi_system, &c, 200, false);
    c.state_mask = 0;
    check(&psi_system, &c, 99, false);
    psi_idle_sample_ns = 0; check(&psi_system, &c, 200, false);
    psi_idle_sample_ns = 100;
    check(&psi_system, &c, 100 + (u64)U32_MAX, false);
    check(&psi_system, &c, 101 + (u64)U32_MAX, false);
    for (unsigned s = 0; s < PSI_NONIDLE; s++) {
        c.times[s] = 1; check(&psi_system, &c, 200, false);
        c.times_prev[PSI_AVGS][s] = 1; check(&psi_system, &c, 200, true);
        psi_system.avg_nr_triggers[s] = 1; check(&psi_system, &c, 200, false);
        psi_system.avg_nr_triggers[s] = 0;
        psi_system.rtpoll_nr_triggers[s] = 1; check(&psi_system, &c, 200, false);
        psi_system.rtpoll_nr_triggers[s] = 0;
    }
    c.times[PSI_NONIDLE] = 500; check(&psi_system, &c, 200, true);
    return 0;
}
'''
        with tempfile.TemporaryDirectory(prefix='pvisor-psi-guard-') as tmp:
            path = Path(tmp)
            (path / 'guard.c').write_text(source)
            subprocess.run([cc, '-std=c11', '-Wall', '-Wextra', '-Werror',
                            str(path / 'guard.c'), '-o', str(path / 'guard')], check=True)
            subprocess.run([str(path / 'guard')], check=True)


if __name__ == '__main__':
    unittest.main()
