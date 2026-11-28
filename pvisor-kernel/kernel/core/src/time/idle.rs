// SPDX-License-Identifier: MPL-2.0

use core::{
    cell::Cell,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use ostd::{
    arch::device::io_port::ReadWriteAccess,
    boot::boot_info,
    cpu::{CpuId, num_cpus},
    io::IoPort,
    irq::IrqLine,
    task::{halt_cpu, halt_cpu_until},
};
use spin::Once;

use super::{clocks, cpu_time_stats::CpuTimeStatsManager};

const PORT: u16 = 0x5050;
const MAGIC: u32 = 0x50564902;
const WAKE_PORT: u16 = 0x5054;
const WAKE_VECTOR: u8 = 0xe0;
const WAKE_MAGIC: u32 = 0x50574901;
// ABI v2 without the wake extension cannot interrupt a sleeping vCPU on arm.
const PROBE_RETRY: Duration = Duration::from_millis(50);
const MAX_IDLE_WAIT: Duration = Duration::from_secs(5);

struct IdleHandoff {
    port: IoPort<u32, ReadWriteAccess>,
    wake_irq: Option<IrqLine>,
}

static HANDOFF: Once<IdleHandoff> = Once::new();
static PROBE_PENDING: AtomicBool = AtomicBool::new(true);
static IN_ONESHOT_WAIT: AtomicBool = AtomicBool::new(false);

pub(crate) fn init() {
    if num_cpus() == 1
        && boot_info()
            .kernel_cmdline
            .split_ascii_whitespace()
            .take_while(|arg| *arg != "--")
            .any(|arg| arg == "pvisor_idle=1")
        && let Ok(port) = IoPort::acquire(PORT)
    {
        let wake_irq = register_wake_irq();
        HANDOFF.call_once(|| IdleHandoff { port, wake_irq });
    }
}

fn register_wake_irq() -> Option<IrqLine> {
    let port = IoPort::<u32, ReadWriteAccess>::acquire(WAKE_PORT).ok()?;
    let mut irq = IrqLine::alloc_specific(WAKE_VECTOR).ok()?;
    irq.on_active(|_| PROBE_PENDING.store(true, Ordering::Relaxed));
    port.write(u32::from(WAKE_VECTOR));
    (port.read() == WAKE_MAGIC).then_some(irq)
}

pub(super) fn in_oneshot_wait() -> bool {
    IN_ONESHOT_WAIT.load(Ordering::Relaxed)
}

pub(crate) fn wait() {
    let handoff = HANDOFF.get();
    let event_timer = ostd::timer::event_timer_enabled();
    if handoff.is_none() && !event_timer {
        halt_cpu();
        return;
    }
    let armed = Cell::new(false);
    halt_cpu_until(
        || {
            IN_ONESHOT_WAIT.store(true, Ordering::Relaxed);
            let Some(handoff) = handoff else {
                // The event timer already holds the earliest wall deadline.
                return MAX_IDLE_WAIT;
            };
            let interrupt_driven = handoff.wake_irq.is_some();
            if !interrupt_driven || PROBE_PENDING.load(Ordering::Relaxed) {
                // The host anchors the deadline before the guest samples its clocks.
                armed.set(handoff.port.read() == MAGIC);
                PROBE_PENDING.store(armed.get(), Ordering::Relaxed);
            }
            let limit = if armed.get() || interrupt_driven {
                MAX_IDLE_WAIT
            } else {
                PROBE_RETRY
            };
            if event_timer && !armed.get() {
                limit
            } else {
                clocks::next_wall_timer().map_or(limit, |delay| delay.min(limit))
            }
        },
        |delay| {
            let millis = delay.as_millis();
            if let Some(handoff) = handoff
                && armed.get()
                && millis > 2
            {
                handoff.port.write(millis as u32);
            }
        },
        |ticks| {
            IN_ONESHOT_WAIT.store(false, Ordering::Relaxed);
            CpuTimeStatsManager::singleton().add_idle_time(CpuId::bsp(), ticks);
            if !event_timer {
                clocks::update_coarse_clock();
            }
        },
    );
}
