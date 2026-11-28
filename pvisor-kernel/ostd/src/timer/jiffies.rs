// SPDX-License-Identifier: MPL-2.0

use core::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use super::TIMER_FREQ;

/// Jiffies is a term used to denote the units of time measurement by the kernel.
///
/// A jiffy represents one tick of the system timer interrupt,
/// whose frequency is equal to [`TIMER_FREQ`] Hz.
#[derive(Clone, Copy, Debug)]
pub struct Jiffies(u64);

pub(crate) static ELAPSED: AtomicU64 = AtomicU64::new(0);

#[cfg(target_arch = "x86_64")]
static TSC_BASE: spin::Once<(u64, u64, u64)> = spin::Once::new();

#[cfg(target_arch = "x86_64")]
pub(crate) fn enable_idle_clock() {
    TSC_BASE.call_once(|| {
        (
            ELAPSED.load(Ordering::Relaxed),
            crate::arch::read_tsc(),
            crate::arch::tsc_freq(),
        )
    });
}

impl Jiffies {
    /// The maximum value of [`Jiffies`].
    pub const MAX: Self = Self(u64::MAX);

    /// Creates a new instance.
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    /// Converts a duration to jiffies, rounding down and saturating on overflow.
    pub fn from_duration(duration: Duration) -> Self {
        Self::new(
            u64::try_from(duration.as_nanos() * u128::from(TIMER_FREQ) / 1_000_000_000)
                .unwrap_or(u64::MAX),
        )
    }

    /// Returns the elapsed time since the system boots up.
    pub fn elapsed() -> Self {
        #[cfg(target_arch = "x86_64")]
        if let Some(&(ticks, tsc, frequency)) = TSC_BASE.get() {
            let cycles = crate::arch::read_tsc().saturating_sub(tsc);
            let elapsed =
                cycles / frequency * TIMER_FREQ + (cycles % frequency) * TIMER_FREQ / frequency;
            return Self::new(ticks + elapsed);
        }
        Self::new(ELAPSED.load(Ordering::Relaxed))
    }

    /// Gets the number of jiffies.
    pub fn as_u64(self) -> u64 {
        self.0
    }

    /// Adds the given number of jiffies, saturating at [`Jiffies::MAX`] on overflow.
    pub fn add(&mut self, jiffies: u64) {
        self.0 = self.0.saturating_add(jiffies);
    }

    /// Gets the [`Duration`] calculated from the jiffies counts.
    pub fn as_duration(self) -> Duration {
        let secs = self.0 / TIMER_FREQ;
        let nanos = ((self.0 % TIMER_FREQ) * 1_000_000_000) / TIMER_FREQ;
        Duration::new(secs, nanos as u32)
    }
}

impl From<Jiffies> for Duration {
    fn from(value: Jiffies) -> Self {
        value.as_duration()
    }
}
