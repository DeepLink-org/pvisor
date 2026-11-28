// SPDX-License-Identifier: MPL-2.0

use ostd::timer;
use spin::Once;

use super::loadavg;
#[cfg(target_arch = "x86_64")]
use crate::time::{Timer, clocks::MonotonicClock, timer::Timeout};

/// The global scheduler statistic singleton
static SCHEDULER_STATS: Once<&'static dyn SchedulerStats> = Once::new();

/// Set the global scheduler statistics singleton.
///
/// This function should be called once to set the scheduler statistics system.
/// It is used to get running stats from the scheduler and to periodically
/// calculate the system load average.
pub(crate) fn set_stats_from_scheduler(scheduler: &'static dyn SchedulerStats) {
    SCHEDULER_STATS.call_once(|| scheduler);

    #[cfg(target_arch = "x86_64")]
    if timer::event_timer_enabled() {
        use alloc::sync::Arc;
        static LOAD_TIMER: Once<Arc<Timer>> = Once::new();
        let interval = timer::Jiffies::new(loadavg::LOAD_FREQ).as_duration();
        let timer = LOAD_TIMER.call_once(|| {
            MonotonicClock::timer_manager().create_timer(|_| {
                loadavg::record_load(nr_queued_and_running().0);
            })
        });
        let mut timer = timer.lock();
        timer.set_interval(interval);
        timer.set_timeout(Timeout::After(interval));
        return;
    }
    // Register a callback to update the load average periodically
    timer::register_callback_on_cpu(|| {
        loadavg::update_loadavg(|| nr_queued_and_running().0);
    });
}

/// The trait for the scheduler statistics.
pub(crate) trait SchedulerStats: Sync + Send {
    /// Returns a tuple with the number of tasks in the runqueues and the number of running tasks.
    ///
    /// We decided to return a tuple instead of having two separate functions to
    /// avoid the overhead of disabling the preemption twice to inspect the scheduler.
    fn nr_queued_and_running(&self) -> (u32, u32);
}

/// Get the amount of tasks in the runqueues and the amount of running tasks.
pub(crate) fn nr_queued_and_running() -> (u32, u32) {
    SCHEDULER_STATS.get().unwrap().nr_queued_and_running()
}
