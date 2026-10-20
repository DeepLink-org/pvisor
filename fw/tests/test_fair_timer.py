"""Source-level scheduler runtime-accounting and timer-stop regression.

Set PVISOR_TEST_FAIR_SOURCE to a patched, uninstrumented kernel/sched/deadline.c.
The C harness compiles the actual update/stop functions with timer/rq stubs;
it does not model kernel concurrency. Real VM tests remain required.
"""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class KernelTimerTests(unittest.TestCase):
    def test_actual_runtime_accounting_and_stop(self):
        path = os.getenv('PVISOR_TEST_FAIR_SOURCE')
        if not path:
            self.skipTest('PVISOR_TEST_FAIR_SOURCE required for actual kernel function test')
        text = Path(path).read_text()
        def function(start):
            a = text.index(start)
            return text[a:text.index('\n}', a)+2]
        update = function('static void update_curr_dl_se(')
        stop = function('void dl_server_stop(')
        source = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
typedef int64_t s64;
struct hrtimer { bool running, queued; };
struct rq;
struct sched_dl_entity {
    struct rq *rq; struct hrtimer dl_timer;
    s64 runtime, deadline; unsigned flags;
    bool dl_yielded, dl_throttled, dl_defer, dl_defer_idle, dl_defer_running;
    bool dl_defer_armed, dl_server_active, dl_overrun, dl_runtime;
};
struct rq { int dl; struct sched_dl_entity fair_server; };
static bool harness_idle;
static int arms, periods, cancellations;
#define unlikely(x) (x)
#define WARN_ON_ONCE(x) assert(!(x))
#define SCHED_FLAG_DL_OVERRUN 1
#define ENQUEUE_REPLENISH 1
#define DEQUEUE_SLEEP 1
#define cpu_of(rq) 0
#define idle_cpu(cpu) harness_idle
#define dl_server(s) true
#define dl_server_active(s) ((s)->dl_server_active)
#define dl_entity_is_special(s) false
#define dl_scaled_delta_exec(r,s,d) (d)
#define dl_runtime_exceeded(s) ((s)->runtime <= 0)
#define dl_task_of(s) 0
#define is_dl_boosted(s) false
#define is_leftmost(s,d) true
#define update_stats_dequeue_dl(...) ((void)0)
#define dequeue_pushable_dl_task(...) ((void)0)
#define enqueue_task_dl(...) ((void)0)
#define enqueue_dl_entity(...) ((void)0)
#define dequeue_dl_entity(...) ((void)0)
#define resched_curr(...) ((void)0)
static inline bool hrtimer_callback_running(struct hrtimer *t) { return t->running; }
static int hrtimer_try_to_cancel(struct hrtimer *t) {
    ++cancellations;
    if (t->running) return -1;
    int old=t->queued; t->queued=false; return old;
}
static void replenish_dl_new_period(struct sched_dl_entity *s, struct rq *r) {
    (void)r; ++periods; s->runtime=50; s->deadline+=1000;
}
static int start_dl_timer(struct sched_dl_entity *s) {
    ++arms; s->dl_timer.queued=true; return 1;
}
''' + update + '\n' + stop + r'''
static void reset(struct rq *r, bool running, bool is_idle) {
    memset(r,0,sizeof(*r)); harness_idle=is_idle; arms=periods=cancellations=0;
    struct sched_dl_entity *s=&r->fair_server;
    s->rq=r; s->runtime=10; s->deadline=1000;
    s->dl_server_active=s->dl_runtime=s->dl_throttled=s->dl_defer=true;
    s->dl_timer.running=running;
}
int main(void) {
    struct rq r; struct sched_dl_entity *s=&r.fair_server;
    /* Previously: update queued a future timer, stop returned -1, queue survived. */
    reset(&r,true,true); update_curr_dl_se(&r,s,20);
    assert(periods==1 && s->runtime==50 && s->deadline==2000);
    assert(s->dl_defer_idle && arms==0 && !s->dl_timer.queued);
    dl_server_stop(s);
    assert(!s->dl_server_active && !s->dl_timer.queued && cancellations==2);
    /* Normal non-idle callback must still rearm and clear prior idle intent. */
    reset(&r,true,false); s->dl_defer_idle=true; update_curr_dl_se(&r,s,20);
    assert(arms==1 && periods==1 && s->runtime==50 && !s->dl_defer_idle);
    /* Idle accounting outside the callback keeps the original expiration. */
    reset(&r,false,true); update_curr_dl_se(&r,s,20);
    assert(arms==1 && periods==1 && s->dl_timer.queued);
    dl_server_stop(s); assert(!s->dl_timer.queued);
    /* Existing idle/no-budget and unexpired budget behavior is unchanged. */
    reset(&r,true,true); s->dl_defer_idle=true; update_curr_dl_se(&r,s,20);
    assert(s->runtime==0 && periods==0 && arms==0);
    reset(&r,true,true); update_curr_dl_se(&r,s,3);
    assert(s->runtime==7 && periods==0 && arms==0);
    reset(&r,true,true); update_curr_dl_se(&r,s,0);
    assert(s->runtime==10 && periods==0 && arms==0);
}
'''
        with tempfile.TemporaryDirectory() as directory:
            src=Path(directory)/'test.c'; binary=Path(directory)/'test'
            src.write_text(source)
            subprocess.run([shutil.which('cc') or 'cc','-std=c11','-Wall','-Werror',str(src),'-o',str(binary)],check=True)
            subprocess.run([str(binary)],check=True)
