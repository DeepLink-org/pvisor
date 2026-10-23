/* GPL-2.0-only. Standalone types/timers, pinned 6.12 averaging reference. */
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
#include <stdio.h>
typedef uint64_t u64;
typedef uint32_t u32;
#define div_u64(a,b) ((a)/(b))
#define U32_MAX UINT32_MAX
#define min_t(t,a,b) ((t)(a)<(t)(b)?(t)(a):(t)(b))
static int psi_defer_irqwork;
static void irq_work_queue(int *work);
#define BIT(n) (1U << (n))
#define WRITE_ONCE(x,y) ((x)=(y))
#define READ_ONCE(x) (x)
#define IS_ENABLED(x) irq_account
#define CONFIG_IRQ_TIME_ACCOUNTING 0
#define local_irq_save(x) ((x)=0)
#define local_irq_restore(x) ((void)(x))
#define lockdep_assert_irqs_disabled() ((void)0)
#define per_cpu_ptr(x,c) (x)
#define static_branch_likely(x) (*(x))
#define atomic_read(x) (*(x))
#define time_after(a,b) ((long)((b)-(a)) < 0)
enum { PSI_IO_SOME, PSI_IO_FULL, PSI_MEM_SOME, PSI_MEM_FULL,
       PSI_CPU_SOME, PSI_CPU_FULL, PSI_NONIDLE, NR_PSI_STATES };
enum { PSI_AVGS, PSI_POLL };
struct timer_list { unsigned long expires; bool pending; };
struct delayed_work { struct timer_list timer; bool pending; unsigned long delay; };
struct psi_group_cpu { u32 state_mask, times[7], times_prev[2][7]; };
struct psi_group {
    struct psi_group_cpu *pcpu;
    struct delayed_work avgs_work;
    u32 avg_nr_triggers[6], rtpoll_nr_triggers[6];
    u64 total[2][6], avg_total[6], avg_last_update, avg_next_update;
    unsigned long avg[6][3];
};
static struct psi_group psi_system;
static struct psi_group_cpu cpu;
static bool psi_idle_defer, psi_idle_quiesce, psi_disabled, irq_account;
static int psi_defer_trigger_inflight;
static unsigned possible = 1;
static unsigned num_possible_cpus(void) { return possible; }
static u64 psi_idle_sample_ns, clock_ns;
static u64 cpu_clock(int cpu_id) { (void)cpu_id; return clock_ns; }
static unsigned long jiffies;
static u64 psi_period = 2004000000ULL;
static unsigned long nsecs_to_jiffies(u64 n) { return n / 4000000; }
static u64 jiffies_to_nsecs(unsigned long j) { return j * 4000000ULL; }
static bool timer_pending(struct timer_list *t) { return t->pending; }
static bool delayed_work_pending(struct delayed_work *w) { return w->pending; }
static bool cancel_delayed_work(struct delayed_work *w) {
    bool pending = w->pending; w->pending = w->timer.pending = false; return pending;
}
static void schedule_delayed_work(struct delayed_work *w, unsigned long d) {
    w->pending = true; w->timer.pending = d != 0; w->delay = d;
}
/* Linux fixed-point load arithmetic; compare old-deadline vs deferred replay
 * under exactly the same arithmetic, not floating point approximations. */
#define FIXED_1 2048UL
static unsigned long calc_load(unsigned long load, unsigned long exp, unsigned long active) {
    unsigned long v = load * exp + active * (FIXED_1 - exp);
    if (active >= load) v += FIXED_1 - 1;
    return v / FIXED_1;
}
static unsigned long fixed_power(unsigned long x, unsigned n) {
    unsigned long result = FIXED_1;
    while (n) {
        if (n & 1) result = (result*x + FIXED_1/2) / FIXED_1;
        n >>= 1;
        if (!n) break;
        x = (x*x + FIXED_1/2) / FIXED_1;
    }
    return result;
}
static void calc_avgs(unsigned long avg[3], unsigned missed, u64 sample, u64 period) {
    const unsigned long exp[3] = {1677,1981,2034};
    for (int i=0; i<3; i++) {
        if (missed) avg[i] = calc_load(avg[i], fixed_power(exp[i], missed), 0);
        avg[i] = calc_load(avg[i], exp[i], (sample*100/period)*FIXED_1);
    }
}
static u64 update_averages(struct psi_group *g, u64 now) {
    assert(now >= g->avg_next_update);
    u64 missed = (now-g->avg_next_update)/psi_period;
    u64 next = g->avg_next_update + (1+missed)*psi_period;
    u64 period = now-(g->avg_last_update + missed*psi_period);
    g->avg_last_update = now;
    for (int s=0;s<PSI_NONIDLE;s++) {
        u32 sample = g->total[PSI_AVGS][s]-g->avg_total[s];
        if (sample>period) sample=period;
        g->avg_total[s]+=sample;
        calc_avgs(g->avg[s], missed, sample, period);
    }
    return next;
}

static void irq_work_queue(int *work) { (void)work; schedule_delayed_work(&psi_system.avgs_work, 0); }
