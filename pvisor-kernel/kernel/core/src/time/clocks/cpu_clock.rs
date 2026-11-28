// SPDX-License-Identifier: MPL-2.0

use alloc::sync::Arc;
use core::time::Duration;

use ostd::{
    sync::{LocalIrqDisabled, SpinLock},
    timer::Jiffies,
};

use crate::time::Clock;

/// A clock used to record the CPU time for processes and threads.
pub(crate) struct CpuClock {
    nanos: SpinLock<u64, LocalIrqDisabled>,
}

/// A profiling clock that contains a user CPU clock and a kernel CPU clock.
///
/// These two clocks record the CPU time in user mode and kernel mode respectively.
/// Reading this clock directly returns the sum of both times.
pub(crate) struct ProfClock {
    user_clock: Arc<CpuClock>,
    kernel_clock: Arc<CpuClock>,
}

impl CpuClock {
    /// Creates a new `CpuClock`. The recorded time is initialized to 0.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            nanos: SpinLock::new(0),
        })
    }

    /// Adds `jiffies` to the original recorded time to update the `CpuClock`.
    pub(crate) fn add_jiffies(&self, jiffies: u64) {
        self.add_nanos(Jiffies::new(jiffies).as_duration().as_nanos() as u64);
    }

    pub(crate) fn add_nanos(&self, nanos: u64) {
        let mut total = self.nanos.lock();
        *total = total.saturating_add(nanos);
    }

    /// Reads the current time of this clock in [`Jiffies`].
    pub(crate) fn read_jiffies(&self) -> Jiffies {
        Jiffies::from_duration(self.read_time())
    }
}

impl Clock for CpuClock {
    fn read_time(&self) -> Duration {
        #[cfg(target_arch = "x86_64")]
        crate::time::cpu_runtime::flush();
        self.accounted_time()
    }
}

impl CpuClock {
    pub(crate) fn accounted_time(&self) -> Duration {
        Duration::from_nanos(*self.nanos.lock())
    }
}

impl ProfClock {
    /// Creates a new `ProfClock`. The recorded time is initialized to 0.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            user_clock: CpuClock::new(),
            kernel_clock: CpuClock::new(),
        })
    }

    /// Returns a reference to the user CPU clock in this profiling clock.
    pub(crate) fn user_clock(&self) -> &Arc<CpuClock> {
        &self.user_clock
    }

    /// Returns a reference to the kernel CPU clock in this profiling clock.
    pub(crate) fn kernel_clock(&self) -> &Arc<CpuClock> {
        &self.kernel_clock
    }
}

impl Clock for ProfClock {
    fn read_time(&self) -> Duration {
        #[cfg(target_arch = "x86_64")]
        crate::time::cpu_runtime::flush();
        self.user_clock.accounted_time() + self.kernel_clock.accounted_time()
    }
}
