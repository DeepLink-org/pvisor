// SPDX-License-Identifier: MPL-2.0

use core::{cell::Cell, time::Duration};

use ostd::{
    boot::boot_info,
    cpu::CpuId,
    cpu_local,
    irq::{DisabledLocalIrqGuard, disable_local},
    task::Task,
    timer,
};

use super::{TimerManager, cpu_time_stats::CpuTimeStatsManager};
use crate::{
    fs::cgroupfs::{CpuStatKind, charge_cpu_time},
    process::{Process, posix_thread::AsPosixThread},
    sched::SchedPolicy,
    thread::AsThread,
};

#[derive(Clone, Copy)]
struct Segment {
    start: u64,
    pending_user: u64,
    pending_kernel: u64,
    user: bool,
    has_timers: bool,
    dispatching: bool,
}

cpu_local! {
    static CURRENT: Cell<Option<Segment>> = Cell::new(None);
}

pub(super) fn init() {
    if timer::event_timer_enabled()
        && boot_info()
            .kernel_cmdline
            .split_ascii_whitespace()
            .take_while(|arg| *arg != "--")
            .any(|arg| arg == "pvisor_cpu_runtime=1")
        && !timer::enable_cpu_runtime_timer(expire)
    {
        ostd::warn!("CPU-runtime deadlines require KVM steal time; retaining accounting tick");
    }
}

#[inline]
fn account(irq: &DisabledLocalIrqGuard, next_user: Option<bool>) -> Option<Segment> {
    let state = CURRENT.get_with(irq);
    let mut segment = state.get()?;
    let now = timer::read_cpu_runtime_ns(irq).max(segment.start);
    let elapsed = now - segment.start;
    segment.start = now;
    if segment.user {
        segment.pending_user = segment.pending_user.saturating_add(elapsed);
    } else {
        segment.pending_kernel = segment.pending_kernel.saturating_add(elapsed);
    }
    if let Some(user) = next_user {
        segment.user = user;
    }
    state.set(Some(segment));
    Some(segment)
}

fn publish(task: &Task, irq: &DisabledLocalIrqGuard, mut segment: Segment) -> Segment {
    let user = core::mem::take(&mut segment.pending_user);
    let kernel = core::mem::take(&mut segment.pending_kernel);
    CURRENT.get_with(irq).set(Some(segment));
    let stats = CpuTimeStatsManager::singleton();
    for (nanos, is_user) in [(user, true), (kernel, false)] {
        if nanos != 0 {
            stats.add_runtime(CpuId::bsp(), is_user, nanos);
        }
    }
    if let Some(thread) = task.as_posix_thread() {
        let counter = |clock: &crate::time::clocks::ProfClock| {
            if user != 0 {
                clock.user_clock().add_nanos(user);
            }
            if kernel != 0 {
                clock.kernel_clock().add_nanos(kernel);
            }
        };
        counter(thread.prof_clock());
        if let Some(process) = thread.weak_process().upgrade() {
            counter(process.prof_clock());
            if user != 0 {
                charge_cpu_time(&process, CpuStatKind::User, user);
            }
            if kernel != 0 {
                charge_cpu_time(&process, CpuStatKind::System, kernel);
            }
        }
    }
    segment
}

pub(crate) fn flush() {
    if timer::cpu_runtime_enabled() {
        let irq = disable_local();
        if let Some(task) = Task::current()
            && let Some(segment) = account(&irq, None)
        {
            publish(&task, &irq, segment);
        }
    }
}

pub(crate) fn user_boundary(task: &Task, user: bool, irq: &DisabledLocalIrqGuard) {
    if !timer::cpu_runtime_enabled() {
        return;
    }
    // Preserve the deadline on kernel entry: its APIC IRQ may already be
    // pending. Check runtime again in the handler before dispatch.
    if account(irq, Some(user)).is_some_and(|segment| user && segment.has_timers) {
        rearm(task, irq);
    }
}

pub(crate) fn switch_in(task: &Task) {
    if !timer::cpu_runtime_enabled()
        || task
            .as_thread()
            .is_some_and(|thread| thread.sched_attr().policy() == SchedPolicy::Idle)
    {
        return;
    }
    let irq = disable_local();
    CURRENT.get_with(&irq).set(Some(Segment {
        start: timer::read_cpu_runtime_ns(&irq),
        pending_user: 0,
        pending_kernel: 0,
        user: false,
        has_timers: false,
        dispatching: false,
    }));
    rearm(task, &irq);
}

pub(crate) fn switch_out(task: &Task, irq: &DisabledLocalIrqGuard) {
    if !timer::cpu_runtime_enabled() {
        return;
    }
    if let Some(segment) = account(irq, None) {
        publish(task, irq, segment);
        CURRENT.get_with(irq).set(None);
        if segment.has_timers {
            timer::set_cpu_runtime_timer(None, irq);
            // The task may not run again. Dispatch timers that became due
            // just before the switch, even if the APIC IRQ is still pending.
            process_due(task);
        }
    }
}

pub(super) fn rearm_current() {
    let irq = disable_local();
    if let Some(task) = Task::current() {
        rearm(&task, &irq);
    }
}

fn with_managers(task: &Task, mut f: impl FnMut([&TimerManager; 2])) {
    let Some(thread) = task.as_posix_thread() else {
        return;
    };
    f(thread.cpu_timer_managers());
    if let Some(process) = thread.weak_process().upgrade() {
        f(process_managers(&process));
    }
}

fn process_managers(process: &Process) -> [&TimerManager; 2] {
    let timers = process.timer_manager();
    [
        timers.prof_timer().timer_manager(),
        timers.virtual_timer().timer_manager(),
    ]
}

fn rearm(task: &Task, irq: &DisabledLocalIrqGuard) {
    let state = CURRENT.get_with(irq);
    let Some(segment) = state.get() else {
        return;
    };
    if segment.dispatching {
        return;
    }
    let mut delay = None;
    let mut has_timers = false;
    with_managers(task, |[prof, user]| {
        let prof = prof.next_delay();
        let user = user.next_delay();
        has_timers |= prof.is_some() || user.is_some();
        delay = delay
            .into_iter()
            .chain(prof)
            .chain(user.filter(|delay| segment.user || *delay == Duration::ZERO))
            .min();
    });
    // next_delay may account the live segment; preserve the new timestamp.
    let mut segment = state.get().unwrap();
    if segment.has_timers || has_timers {
        timer::set_cpu_runtime_timer(delay, irq);
        segment.has_timers = has_timers;
        state.set(Some(segment));
    }
}

fn process_due(task: &Task) {
    with_managers(task, |managers| {
        for manager in managers {
            manager.process_expired_timers();
        }
    });
}

fn expire() {
    let irq = disable_local();
    let Some(task) = Task::current() else {
        return;
    };
    if let Some(segment) = account(&irq, None) {
        let state = CURRENT.get_with(&irq);
        let mut segment = publish(&task, &irq, segment);
        segment.dispatching = true;
        state.set(Some(segment));
        process_due(&task);
        let mut segment = state.get().unwrap();
        segment.dispatching = false;
        state.set(Some(segment));
        rearm(&task, &irq);
    }
}
