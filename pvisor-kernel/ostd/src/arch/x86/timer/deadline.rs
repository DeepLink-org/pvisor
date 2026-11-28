// SPDX-License-Identifier: MPL-2.0

use core::{cell::Cell, time::Duration};

use spin::Once;

use super::apic::{configure_oneshot, deadline_after, program_oneshot};
use crate::{
    arch::{kernel::kvm_clock::RuntimeClock, read_tsc, trap::TrapFrame, tsc_freq},
    cpu_local,
    irq::{DisabledLocalIrqGuard, disable_local},
    timer::{TIMER_FREQ, call_timer_callback_functions},
};

static ON_EVENT: Once<fn()> = Once::new();
static CPU_RUNTIME: Once<(RuntimeClock, fn())> = Once::new();

cpu_local! {
    static TIMER: Cell<Deadlines> = Cell::new(Deadlines {
        tick: None,
        event: None,
        cpu_timer: None,
        preemption: None,
        idle: None,
        programmed: None,
    });
}

#[derive(Clone, Copy)]
struct Deadlines {
    tick: Option<u64>,
    event: Option<u64>,
    cpu_timer: Option<u64>,
    preemption: Option<u64>,
    idle: Option<u64>,
    programmed: Option<u64>,
}

impl Deadlines {
    fn change(irq: &DisabledLocalIrqGuard, f: impl FnOnce(&mut Self)) {
        Self::update(irq, false, f);
    }

    fn update(
        irq: &DisabledLocalIrqGuard,
        keep_earlier_interrupt: bool,
        f: impl FnOnce(&mut Self),
    ) {
        let cell = TIMER.get_with(irq);
        let mut state = cell.get();
        f(&mut state);
        let next = state
            .tick
            .into_iter()
            .chain(state.event)
            .chain(state.cpu_timer)
            .chain(state.preemption)
            .chain(state.idle)
            .min();
        let keep = keep_earlier_interrupt
            && state
                .programmed
                .is_some_and(|armed| next.is_none_or(|next| armed <= next));
        if next != state.programmed && !keep {
            program_oneshot(next, irq);
            state.programmed = next;
        }
        cell.set(state);
    }
}

pub(crate) fn enable_event_timer(on_event: fn()) {
    let irq = disable_local();
    assert_eq!(crate::cpu::num_cpus(), 1);
    assert!(!event_timer_enabled());
    ON_EVENT.call_once(|| on_event);
    crate::timer::enable_idle_clock();
    configure_oneshot(&irq);
    leave_idle(&irq);
}

pub(crate) fn event_timer_enabled() -> bool {
    ON_EVENT.get().is_some()
}

pub(crate) fn set_event_timer(delay: Option<Duration>) {
    let irq = disable_local();
    Deadlines::change(&irq, |state| state.event = delay.map(deadline_after));
}

pub(crate) fn enable_cpu_runtime_timer(on_event: fn()) -> bool {
    let irq = disable_local();
    assert!(event_timer_enabled() && CPU_RUNTIME.get().is_none());
    let Some(clock) = RuntimeClock::setup() else {
        return false;
    };
    CPU_RUNTIME.call_once(|| (clock, on_event));
    Deadlines::change(&irq, |state| state.tick = None);
    true
}

pub(crate) fn cpu_runtime_enabled() -> bool {
    CPU_RUNTIME.get().is_some()
}

pub(crate) fn read_cpu_runtime_ns(_irq: &DisabledLocalIrqGuard) -> u64 {
    CPU_RUNTIME.get().unwrap().0.read_ns()
}

pub(crate) fn set_cpu_runtime_timer(delay: Option<Duration>, irq: &DisabledLocalIrqGuard) {
    Deadlines::change(irq, |state| state.cpu_timer = delay.map(deadline_after));
}

pub(crate) fn set_preemption_deadline(deadline: Option<u64>, irq: &DisabledLocalIrqGuard) {
    // IPC may block before the slice expires. Keep an earlier interrupt
    // armed and revalidate the runqueue deadline when it arrives.
    Deadlines::update(irq, cpu_runtime_enabled(), |state| {
        state.preemption = deadline
    });
}

pub(super) fn enter_idle(retry: Option<u64>, irq: &DisabledLocalIrqGuard) {
    Deadlines::change(irq, |state| {
        state.tick = None;
        state.idle = retry;
    });
}

pub(super) fn leave_idle(irq: &DisabledLocalIrqGuard) {
    Deadlines::change(irq, |state| {
        state.idle = None;
        if !cpu_runtime_enabled() {
            state.tick = Some(read_tsc() + tsc_freq() / TIMER_FREQ);
        }
    });
}

pub(super) fn handle_interrupt(trapframe: &TrapFrame) -> bool {
    let Some(on_event) = ON_EVENT.get() else {
        return false;
    };
    let irq = disable_local();
    let now = read_tsc();
    let mut tick_due = false;
    let mut event_due = false;
    let mut cpu_due = false;
    let mut preemption_due = false;
    Deadlines::change(&irq, |state| {
        // A count-mode interrupt can arrive before its TSC target. Rearm any
        // undelivered deadline, even when its absolute target has not changed.
        state.programmed = None;
        if state.tick.is_some_and(|tick| tick <= now) {
            tick_due = true;
            state.tick = Some(now + tsc_freq() / TIMER_FREQ);
        }
        if state.event.is_some_and(|event| event <= now) {
            event_due = true;
            state.event = None;
        }
        if state.cpu_timer.is_some_and(|deadline| deadline <= now) {
            cpu_due = true;
            state.cpu_timer = None;
        }
        if state.preemption.is_some_and(|deadline| deadline <= now) {
            preemption_due = true;
            state.preemption = None;
        }
        if state.idle.is_some_and(|idle| idle <= now) {
            state.idle = None;
        }
    });
    if tick_due {
        call_timer_callback_functions(trapframe);
    }
    if cpu_due {
        (CPU_RUNTIME.get().unwrap().1)();
    }
    if preemption_due {
        crate::task::scheduler::check_preemption();
    }
    if event_due {
        on_event();
    }
    true
}
