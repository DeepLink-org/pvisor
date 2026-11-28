// SPDX-License-Identifier: MPL-2.0

use core::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

use crate::{
    arch::{
        kernel::apic::{self, Apic, DivideConfig},
        timer::pit::OperatingMode,
        trap::TrapFrame,
        tsc_freq,
    },
    info,
    irq::{DisabledLocalIrqGuard, IrqLine},
    task::disable_preempt,
    timer::TIMER_FREQ,
};

/// Initializes APIC with TSC-deadline mode or periodic mode.
///
/// Return the corresponding [`IrqLine`] for the system timer.
pub(super) fn init_on_bsp() -> IrqLine {
    if is_tsc_deadline_mode_supported() {
        init_deadline_mode_config();
    } else {
        init_periodic_mode_config();
    }

    let timer_irq = IrqLine::alloc().unwrap();
    init_timer(&timer_irq);
    timer_irq
}

/// Initializes APIC timer on AP.
///
/// The caller should provide the [`IrqLine`] for the system timer.
pub(super) fn init_on_ap(timer_irq: &IrqLine) {
    init_timer(timer_irq);
}

/// A callback that needs to be called on timer interrupt.
pub(super) fn timer_callback() {
    use x86::msr::{IA32_TSC_DEADLINE, wrmsr};

    if IDLE_TIMER.load(Ordering::Relaxed) {
        return;
    }
    match CONFIG.get().expect("ACPI timer config is not initialized") {
        Config::DeadlineMode { tsc_interval } => {
            let tsc_value = crate::arch::read_tsc();
            let next_tsc_value = tsc_interval + tsc_value;
            unsafe { wrmsr(IA32_TSC_DEADLINE, next_tsc_value) };
        }
        Config::PeriodicMode { .. } => {}
    }
}

static IDLE_TIMER: AtomicBool = AtomicBool::new(false);

pub(super) fn deadline_after(delay: Duration) -> u64 {
    let cycles = (delay.as_nanos() * u128::from(tsc_freq())).div_ceil(1_000_000_000);
    crate::arch::read_tsc().saturating_add(cycles.clamp(1, u128::from(u64::MAX)) as u64)
}

pub(super) fn configure_oneshot(irq: &DisabledLocalIrqGuard) {
    if matches!(*CONFIG.get().unwrap(), Config::PeriodicMode { .. }) {
        let apic = apic::get_or_init(irq);
        apic.set_timer_init_count(0);
        apic.set_lvt_timer(u64::from(super::TIMER_IRQ.get().unwrap().num()));
    }
}

pub(super) fn program_oneshot(deadline: Option<u64>, irq: &DisabledLocalIrqGuard) {
    match *CONFIG.get().unwrap() {
        Config::DeadlineMode { .. } => {
            // SAFETY: TSC-deadline support was checked when selecting this mode.
            unsafe { x86::msr::wrmsr(x86::msr::IA32_TSC_DEADLINE, deadline.unwrap_or(0)) };
        }
        Config::PeriodicMode { init_count } => {
            let count = deadline.map_or(0, |deadline| {
                let cycles = deadline.saturating_sub(crate::arch::read_tsc());
                (u128::from(cycles) * u128::from(init_count) * u128::from(TIMER_FREQ))
                    .div_ceil(u128::from(tsc_freq()))
                    .clamp(1, u128::from(u32::MAX)) as u64
            });
            apic::get_or_init(irq).set_timer_init_count(count);
        }
    }
}

pub(crate) fn start_idle_timer(delay: Duration, irq: &DisabledLocalIrqGuard) {
    let deadline = deadline_after(delay);
    if super::deadline::event_timer_enabled() {
        super::deadline::enter_idle(deadline, irq);
        return;
    }
    IDLE_TIMER.store(true, Ordering::Relaxed);
    configure_oneshot(irq);
    program_oneshot(Some(deadline), irq);
}

pub(crate) fn stop_idle_timer(irq: &DisabledLocalIrqGuard) {
    if super::deadline::event_timer_enabled() {
        super::deadline::leave_idle(irq);
        return;
    }
    IDLE_TIMER.store(false, Ordering::Relaxed);
    match *CONFIG.get().unwrap() {
        Config::DeadlineMode { .. } => timer_callback(),
        Config::PeriodicMode { init_count } => {
            let apic = apic::get_or_init(irq);
            apic.set_timer_init_count(0);
            init_periodic_mode(apic, super::TIMER_IRQ.get().unwrap(), init_count);
        }
    }
}

/// Determines if the current system supports tsc_deadline mode APIC timer
fn is_tsc_deadline_mode_supported() -> bool {
    use crate::arch::cpu::extension::{IsaExtensions, has_extensions};

    has_extensions(IsaExtensions::TSC_DEADLINE)
}

fn init_timer(timer_irq: &IrqLine) {
    let preempt_guard = disable_preempt();
    let apic = apic::get_or_init(&preempt_guard as _);

    match CONFIG.get().expect("ACPI timer config is not initialized") {
        Config::DeadlineMode { .. } => {
            init_deadline_mode(apic, timer_irq);
        }
        Config::PeriodicMode { init_count } => {
            init_periodic_mode(apic, timer_irq, *init_count);
        }
    }
}

fn init_deadline_mode(apic: &dyn Apic, timer_irq: &IrqLine) {
    // Enable TSC deadline mode
    apic.set_lvt_timer(timer_irq.num() as u64 | (1 << 18));

    timer_callback();
}

fn init_periodic_mode(apic: &dyn Apic, timer_irq: &IrqLine, init_count: u64) {
    // Enable periodic mode
    apic.set_timer_init_count(init_count);
    apic.set_lvt_timer(timer_irq.num() as u64 | (1 << 17));
    apic.set_timer_div_config(DivideConfig::Divide64);
}

static CONFIG: spin::Once<Config> = spin::Once::new();

enum Config {
    DeadlineMode { tsc_interval: u64 },
    PeriodicMode { init_count: u64 },
}

fn init_deadline_mode_config() {
    info!("Enable APIC TSC deadline mode");

    let tsc_interval = tsc_freq() / TIMER_FREQ;
    CONFIG.call_once(|| Config::DeadlineMode { tsc_interval });
}

fn init_periodic_mode_config() {
    info!("Enable APIC periodic mode");

    // Allocate IRQ
    let mut irq = IrqLine::alloc().unwrap();
    irq.on_active(pit_callback);

    // Enable PIT
    super::pit::init(OperatingMode::RateGenerator);
    let irq = super::pit::enable_interrupt(irq);

    // Set APIC timer count
    let preempt_guard = disable_preempt();
    let apic = apic::get_or_init(&preempt_guard as _);
    apic.set_timer_div_config(DivideConfig::Divide64);
    apic.set_timer_init_count(0xFFFF_FFFF);

    // Wait until `CONFIG` is ready
    loop {
        crate::arch::irq::enable_local_and_halt();

        // Disable local IRQs so they won't come after checking `CONFIG`
        // but before halting the CPU.
        crate::arch::irq::disable_local();

        if CONFIG.is_completed() {
            break;
        }
    }

    // Disable PIT
    drop(irq);

    fn pit_callback(_trap_frame: &TrapFrame) {
        static IN_TIME: AtomicU64 = AtomicU64::new(0);
        static APIC_FIRST_COUNT: AtomicU64 = AtomicU64::new(0);
        // Set a certain times of callbacks to calculate the frequency
        // The number of callbacks needed to calculate the APIC timer frequency.
        // This is set to 1/10th of the TIMER_FREQ to ensure enough samples for accurate calculation.
        const CALLBACK_TIMES: u64 = TIMER_FREQ / 10;

        let preempt_guard = disable_preempt();
        let apic = apic::get_or_init(&preempt_guard as _);

        let apic_current_count = 0xFFFF_FFFF - apic.timer_current_count();

        if IN_TIME.load(Ordering::Relaxed) < CALLBACK_TIMES || CONFIG.is_completed() {
            if IN_TIME.load(Ordering::Relaxed) == 0 {
                APIC_FIRST_COUNT.store(apic_current_count, Ordering::Relaxed);
            }
            IN_TIME.fetch_add(1, Ordering::Relaxed);
            return;
        }

        // Stop APIC Timer
        apic.set_timer_init_count(0);

        let apic_first_count = APIC_FIRST_COUNT.load(Ordering::Relaxed);
        let apic_init_count = (apic_current_count - apic_first_count) / CALLBACK_TIMES;
        info!(
            "APIC timer: first {:#x}, current {:#x}, init {:#x}",
            apic_first_count, apic_current_count, apic_init_count,
        );
        CONFIG.call_once(|| Config::PeriodicMode {
            init_count: apic_init_count,
        });
    }
}
