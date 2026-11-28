// SPDX-License-Identifier: MPL-2.0

//! This module provide a instance of `ClockSource` based on TSC.

use alloc::sync::Arc;

use ostd::arch::{read_tsc, tsc_freq};
use spin::Once;

use crate::{
    START_TIME,
    clocksource::{ClockSource, Instant},
};

/// An instance of the TSC clocksource.
pub(super) static CLOCK: Once<Arc<ClockSource>> = Once::new();

const MAX_DELAY_SECS: u64 = 100;

/// Initializes the TSC clocksource module.
pub(super) fn init() {
    init_clock();
    calibrate();
}

fn init_clock() {
    CLOCK.call_once(|| {
        Arc::new(ClockSource::new(
            tsc_freq(),
            MAX_DELAY_SECS,
            Arc::new(read_tsc),
        ))
    });
}

/// Calibrates the TSC and system time based on the RTC time.
fn calibrate() {
    let clock = CLOCK.get().unwrap();
    let cycles = clock.read_cycles();
    clock.calibrate(cycles);
    START_TIME.call_once(|| crate::RTC_DRIVER.get().unwrap().read_rtc());
}

/// Reads an `Instant` of the TSC clocksource.
pub(super) fn read_instant() -> Instant {
    let clock = CLOCK.get().unwrap();
    clock.read_instant()
}
