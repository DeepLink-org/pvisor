// SPDX-License-Identifier: MPL-2.0

use core::{cell::Cell, time::Duration};

use spin::Once;

use super::apic::{configure_oneshot, deadline_after, program_oneshot};
use crate::{
    arch::{read_tsc, trap::TrapFrame, tsc_freq},
    cpu_local,
    irq::{DisabledLocalIrqGuard, disable_local},
    timer::{TIMER_FREQ, call_timer_callback_functions},
};

static ON_EVENT: Once<fn()> = Once::new();

cpu_local! {
    static TIMER: Cell<Deadlines> = Cell::new(Deadlines {
        tick: None,
        event: None,
        idle: None,
        programmed: None,
    });
}

#[derive(Clone, Copy)]
struct Deadlines {
    tick: Option<u64>,
    event: Option<u64>,
    idle: Option<u64>,
    programmed: Option<u64>,
}

impl Deadlines {
    fn change(irq: &DisabledLocalIrqGuard, f: impl FnOnce(&mut Self)) {
        let cell = TIMER.get_with(irq);
        let mut state = cell.get();
        f(&mut state);
        let next = state
            .tick
            .into_iter()
            .chain(state.event)
            .chain(state.idle)
            .min();
        if next != state.programmed {
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

pub(super) fn enter_idle(deadline: u64, irq: &DisabledLocalIrqGuard) {
    Deadlines::change(irq, |state| {
        state.tick = None;
        state.idle = Some(deadline);
    });
}

pub(super) fn leave_idle(irq: &DisabledLocalIrqGuard) {
    Deadlines::change(irq, |state| {
        state.idle = None;
        state.tick = Some(read_tsc() + tsc_freq() / TIMER_FREQ);
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
        if state.idle.is_some_and(|idle| idle <= now) {
            state.idle = None;
        }
    });
    if tick_due {
        call_timer_callback_functions(trapframe);
    }
    if event_due {
        on_event();
    }
    true
}
