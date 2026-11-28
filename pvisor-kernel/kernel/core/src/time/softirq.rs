// SPDX-License-Identifier: MPL-2.0

use alloc::{boxed::Box, vec, vec::Vec};

use aster_softirq::{SoftIrqLine, softirq_id::TIMER_SOFTIRQ_ID};
use ostd::{sync::RcuOption, timer};

#[expect(clippy::type_complexity)]
static TIMER_SOFTIRQ_CALLBACKS: RcuOption<Box<Vec<fn()>>> = RcuOption::new_none();

pub(super) fn init() {
    SoftIrqLine::get(TIMER_SOFTIRQ_ID).enable(timer_softirq_handler);

    #[cfg(target_arch = "x86_64")]
    if ostd::cpu::num_cpus() == 1
        && ostd::boot::boot_info()
            .kernel_cmdline
            .split_ascii_whitespace()
            .take_while(|arg| *arg != "--")
            .any(|arg| arg == "pvisor_wall_deadline=1")
    {
        timer::enable_event_timer(|| {
            WALL_EVENT_PENDING.store(true, core::sync::atomic::Ordering::Relaxed);
            raise_timer_softirq();
        });
        return;
    }

    register_callback(super::clocks::update_coarse_clock);
    timer::register_callback_on_cpu(raise_timer_softirq);
}

fn raise_timer_softirq() {
    SoftIrqLine::get(TIMER_SOFTIRQ_ID).raise();
}

#[cfg(target_arch = "x86_64")]
static WALL_EVENT_PENDING: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[cfg(target_arch = "x86_64")]
pub(super) fn rearm_wall_timer() {
    let _irq = ostd::irq::disable_local();
    if !WALL_EVENT_PENDING.load(core::sync::atomic::Ordering::Relaxed) {
        timer::set_event_timer(super::clocks::next_wall_timer());
    }
}

/// Registers a function that will be executed during timer softirq.
pub(super) fn register_callback(func: fn()) {
    loop {
        let callbacks = TIMER_SOFTIRQ_CALLBACKS.read();
        match callbacks.get() {
            // Initialized, copy the vector, push the function and update.
            Some(callbacks_vec) => {
                let mut callbacks_cloned = callbacks_vec.clone();
                callbacks_cloned.push(func);
                if callbacks.compare_exchange(Some(callbacks_cloned)).is_ok() {
                    break;
                }
            }
            // Uninitialized, initialize it.
            None => {
                if callbacks
                    .compare_exchange(Some(Box::new(vec![func])))
                    .is_ok()
                {
                    break;
                }
            }
        }
        // Contention on initialization or pushing, retry.
        core::hint::spin_loop();
    }
}

fn timer_softirq_handler() {
    let callbacks = TIMER_SOFTIRQ_CALLBACKS.read();
    if let Some(callbacks) = callbacks.get() {
        for callback in callbacks.iter() {
            (callback)();
        }
    }
    #[cfg(target_arch = "x86_64")]
    if timer::event_timer_enabled() {
        let _irq = ostd::irq::disable_local();
        WALL_EVENT_PENDING.store(false, core::sync::atomic::Ordering::Relaxed);
        rearm_wall_timer();
    }
}
