// SPDX-License-Identifier: MPL-2.0

//! The timer support.

mod jiffies;

use alloc::{boxed::Box, vec::Vec};
use core::{cell::RefCell, sync::atomic::Ordering};

pub use jiffies::Jiffies;

#[cfg(target_arch = "x86_64")]
pub(crate) use jiffies::enable_idle_clock;

use crate::{
    arch::trap::TrapFrame,
    cpu::{CpuId, PinCurrentCpu},
    cpu_local, irq,
};

/// The timer frequency in Hz.
///
/// Here we choose 1000Hz since 1000Hz is easier for unit conversion and convenient for timer.
/// What's more, the frequency cannot be set too high or too low, 1000Hz is a modest choice.
///
/// For system performance reasons, this rate cannot be set too high, otherwise most of the time is
/// spent in executing timer code.
pub const TIMER_FREQ: u64 = 1000;

/// Enables the APIC event timer alongside the busy-time tick on x86-64.
///
/// Call once after APIC initialization on a single-vCPU system. `on_event`
/// runs with local interrupts disabled when the requested deadline expires.
/// It must not block or enable interrupts.
///
/// # Panics
///
/// Panics on SMP or if called twice.
#[cfg(target_arch = "x86_64")]
pub fn enable_event_timer(on_event: fn()) {
    crate::arch::timer::enable_event_timer(on_event);
}

/// Reports whether the x86-64 APIC event timer is enabled.
#[cfg(target_arch = "x86_64")]
pub fn event_timer_enabled() -> bool {
    crate::arch::timer::event_timer_enabled()
}

/// Replaces the current APIC event deadline with a relative delay, or disarms
/// that deadline with `None`. This does not change the busy-time tick.
///
/// # Panics
///
/// Panics unless [`enable_event_timer`] was called.
#[cfg(target_arch = "x86_64")]
pub fn set_event_timer(delay: Option<core::time::Duration>) {
    assert!(event_timer_enabled());
    crate::arch::timer::set_event_timer(delay);
}

/// Replaces the compatibility tick with a CPU-runtime event deadline.
///
/// Call once after enabling the event timer, before registering tick users.
/// Returns `false` and leaves the tick unchanged without KVM steal time.
/// The callback runs with local interrupts disabled and must not block.
#[cfg(target_arch = "x86_64")]
pub fn enable_cpu_runtime_timer(on_event: fn()) -> bool {
    crate::arch::timer::enable_cpu_runtime_timer(on_event)
}

/// Reports whether execution-interval accounting has replaced the tick.
#[cfg(target_arch = "x86_64")]
pub fn cpu_runtime_enabled() -> bool {
    crate::arch::timer::cpu_runtime_enabled()
}

/// Samples TSC time minus KVM's reported vCPU scheduling delay, in nanoseconds.
///
/// Requires initialized CPU runtime support and disabled local interrupts.
#[cfg(target_arch = "x86_64")]
pub fn read_cpu_runtime_ns(irq: &crate::irq::DisabledLocalIrqGuard) -> u64 {
    crate::arch::timer::read_cpu_runtime_ns(irq)
}

/// Arms a wall-time estimate of a CPU timer's remaining runtime, or disarms it.
///
/// Call only with CPU runtime support enabled. Recheck the actual CPU clock
/// at expiry: a vCPU descheduled by the host may not have run long enough.
#[cfg(target_arch = "x86_64")]
pub fn set_cpu_runtime_timer(
    delay: Option<core::time::Duration>,
    irq: &crate::irq::DisabledLocalIrqGuard,
) {
    assert!(cpu_runtime_enabled());
    crate::arch::timer::set_cpu_runtime_timer(delay, irq);
}

type InterruptCallback = Box<dyn Fn() + Sync + Send>;

cpu_local! {
    static INTERRUPT_CALLBACKS: RefCell<Vec<InterruptCallback>> = RefCell::new(Vec::new());
}

/// Registers a function that will be executed during the timer interrupt on the current CPU.
pub fn register_callback_on_cpu<F>(func: F)
where
    F: Fn() + Sync + Send + 'static,
{
    let irq_guard = irq::disable_local();
    INTERRUPT_CALLBACKS
        .get_with(&irq_guard)
        .borrow_mut()
        .push(Box::new(func));
}

pub(crate) fn call_timer_callback_functions(_: &TrapFrame) {
    let irq_guard = irq::disable_local();

    if irq_guard.current_cpu() == CpuId::bsp() {
        jiffies::ELAPSED.fetch_add(1, Ordering::Relaxed);
    }

    let callbacks_guard = INTERRUPT_CALLBACKS.get_with(&irq_guard);
    for callback in callbacks_guard.borrow().iter() {
        (callback)();
    }
    drop(callbacks_guard);
}
