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
/// interrupts. `prepare` returns a relative delay, at most five seconds.
/// `offer` receives that delay after programming and before the halt.
/// `finish` runs after restoring the tick and receives the elapsed idle ticks.
///
/// # Panics
///
/// Panics outside sleepable task context, on SMP, or for an excessive delay.
#[cfg(target_arch = "x86_64")]
pub fn halt_cpu_until(
    prepare: impl FnOnce() -> core::time::Duration,
    offer: impl FnOnce(core::time::Duration),
    finish: impl FnOnce(u64),
) {
    crate::task::atomic_mode::might_sleep();
    assert_eq!(crate::cpu::num_cpus(), 1);

    let preempt = disable_preempt();
    let irq = crate::irq::disable_local();
    if !cpu_local::need_preempt() {
        crate::timer::enable_idle_clock();
        let start = crate::timer::Jiffies::elapsed().as_u64();
        let delay = prepare();
        assert!(delay <= core::time::Duration::from_secs(5));
        crate::arch::timer::start_idle_timer(delay, &irq);
        offer(delay);
        core::mem::forget(irq);
        crate::arch::irq::enable_local_and_halt();
        let irq = crate::irq::disable_local();
        let elapsed = crate::timer::Jiffies::elapsed()
            .as_u64()
            .saturating_sub(start);
        crate::arch::timer::stop_idle_timer(&irq);
        finish(elapsed);
    } else {
        drop(irq);
    }
    drop(preempt);
    super::scheduler::might_preempt();
}
