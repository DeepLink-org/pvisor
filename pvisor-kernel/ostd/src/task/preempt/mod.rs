// SPDX-License-Identifier: MPL-2.0

pub(super) mod cpu_local;
mod guard;

pub use self::guard::{DisabledPreemptGuard, disable_preempt};

/// Halts the CPU until interrupts if no preemption is required.
///
/// This function will return if:
///  - preemption is required when calling this function,
///  - preemption is required during halting the CPU, or
///  - interrupts occur during halting the CPU.
///
/// This function will perform preemption before returning if
/// preemption is required.
///
/// # Panics
///
/// This function will panic if it is called in the atomic mode
/// ([`crate::task::atomic_mode`]).
#[track_caller]
pub fn halt_cpu() {
    crate::task::atomic_mode::might_sleep();

    let irq_guard = crate::irq::disable_local();

    if cpu_local::need_preempt() {
        drop(irq_guard);
    } else {
        core::mem::forget(irq_guard);
        // IRQs were previously enabled (checked by `might_sleep`). So we can re-enable them now.
        crate::arch::irq::enable_local_and_halt();
    }

    super::scheduler::might_preempt();
}

/// Waits in the single-vCPU idle task with a one-shot timer, restoring the
/// scheduler tick before preemption.
///
/// The callbacks run with local IRQs disabled and must not block or enable
/// interrupts. `prepare` returns `(local_delay, offer_delay)` relative to now.
/// The local delay may be omitted only when the event timer is enabled.
/// Both delays are at most five seconds. `offer` is called with the offer
/// delay, if any, after programming and before the halt.
/// `finish` runs after restoring the tick and receives the elapsed idle ticks.
/// With the event timer enabled, interrupts that leave no task runnable
/// cause another wait without restoring the tick.
///
/// # Panics
///
/// Panics outside sleepable task context, on SMP, or for an excessive delay.
#[cfg(target_arch = "x86_64")]
pub fn halt_cpu_until(
    mut prepare: impl FnMut() -> (Option<core::time::Duration>, Option<core::time::Duration>),
    mut offer: impl FnMut(core::time::Duration),
    finish: impl FnOnce(u64),
) {
    use core::time::Duration;

    crate::task::atomic_mode::might_sleep();
    assert_eq!(crate::cpu::num_cpus(), 1);

    let event_timer = crate::timer::event_timer_enabled();
    let preempt = disable_preempt();
    let mut irq = crate::irq::disable_local();
    if !cpu_local::need_preempt() {
        crate::timer::enable_idle_clock();
        let start = crate::timer::Jiffies::elapsed().as_u64();
        loop {
            let (local_delay, offer_delay) = prepare();
            assert!(
                local_delay
                    .into_iter()
                    .chain(offer_delay)
                    .all(|delay| delay <= Duration::from_secs(5))
            );
            crate::arch::timer::start_idle_timer(local_delay, &irq);
            if let Some(delay) = offer_delay {
                offer(delay);
            }
            core::mem::forget(irq);
            crate::arch::irq::enable_local_and_halt();
            irq = crate::irq::disable_local();
            if !event_timer || cpu_local::need_preempt() {
                break;
            }
        }
        let elapsed = crate::timer::Jiffies::elapsed()
            .as_u64()
            .saturating_sub(start);
        crate::arch::timer::stop_idle_timer(&irq);
        finish(elapsed);
    }
    drop(irq);
    drop(preempt);
    super::scheduler::might_preempt();
}
