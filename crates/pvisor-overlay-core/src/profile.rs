//! Opt-in aggregate filesystem profiling. Nested spans are inclusive and must
//! not be summed. Disabled profiles do not read clocks, lock, or allocate per
//! operation. No filenames or file contents are recorded.
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Default, Serialize)]
pub struct Measurement {
    pub calls: u64,
    pub total_ns: u64,
    pub units: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProfileReport {
    pub schema: u32,
    pub pid: u32,
    pub component: String,
    pub instance: u64,
    pub inclusive_spans: bool,
    /// Periodic records are cumulative checkpoints, not a completed run.
    pub final_record: bool,
    pub measurements: BTreeMap<String, Measurement>,
}

#[derive(Debug)]
struct State {
    component: String,
    instance: u64,
    emit_on_drop: bool,
    measurements: Mutex<BTreeMap<&'static str, Measurement>>,
    last_emission: Mutex<Instant>,
}
impl State {
    fn report(&self) -> ProfileReport {
        ProfileReport {
            schema: 1,
            pid: std::process::id(),
            component: self.component.clone(),
            instance: self.instance,
            inclusive_spans: true,
            final_record: false,
            measurements: self
                .measurements
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .map(|(k, v)| ((*k).into(), v.clone()))
                .collect(),
        }
    }

    fn emit(&self, final_record: bool) {
        let mut report = self.report();
        report.final_record = final_record;
        if let Ok(json) = serde_json::to_string(&report) {
            eprintln!("pvisor-fs-profile {json}");
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        if self.emit_on_drop {
            self.emit(true);
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Profile(Option<Arc<State>>);
impl Profile {
    pub fn from_env(component: &str) -> Self {
        if std::env::var("PVISOR_FS_PROFILE").as_deref() == Ok("1") {
            Self::configured(component, true)
        } else {
            Self::default()
        }
    }
    /// Explicit opt-in for deterministic tests and benchmarks. Does not log.
    pub fn enabled(component: &str) -> Self {
        Self::configured(component, false)
    }
    fn configured(component: &str, emit_on_drop: bool) -> Self {
        Self(Some(Arc::new(State {
            component: component.into(),
            instance: NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed),
            emit_on_drop,
            measurements: Mutex::new(BTreeMap::new()),
            last_emission: Mutex::new(Instant::now()),
        })))
    }
    pub fn report(&self) -> Option<ProfileReport> {
        self.0.as_ref().map(|s| s.report())
    }
    pub fn span(&self, label: &'static str) -> Span<'_> {
        Span {
            profile: self,
            label,
            started: self.0.as_ref().map(|_| Instant::now()),
        }
    }
    pub fn add(&self, label: &'static str, units: u64) {
        if let Some(state) = &self.0 {
            let mut measurements = state.measurements.lock().unwrap_or_else(|p| p.into_inner());
            let value = measurements.entry(label).or_default();
            value.units = value.units.saturating_add(units);
        }
    }

    /// Emit a cumulative checkpoint when explicitly requested, e.g. at freeze.
    pub fn emit_checkpoint(&self) {
        if let Some(state) = &self.0
            && state.emit_on_drop
        {
            state.emit(false);
        }
    }
}
pub struct Span<'a> {
    profile: &'a Profile,
    label: &'static str,
    started: Option<Instant>,
}
impl Drop for Span<'_> {
    fn drop(&mut self) {
        if let (Some(state), Some(started)) = (&self.profile.0, self.started) {
            let elapsed = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            let mut measurements = state.measurements.lock().unwrap_or_else(|p| p.into_inner());
            let value = measurements.entry(self.label).or_default();
            value.calls = value.calls.saturating_add(1);
            value.total_ns = value.total_ns.saturating_add(elapsed);
            drop(measurements);
            // VMM shutdown can use _exit, bypassing destructors. Periodic
            // cumulative records retain evidence without logging every request.
            if state.emit_on_drop {
                let mut last = state
                    .last_emission
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                if last.elapsed() >= Duration::from_millis(250) {
                    *last = Instant::now();
                    drop(last);
                    state.emit(false);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_and_shared_profiles_preserve_counts() {
        let disabled = Profile::default();
        disabled.add("bytes", 7);
        drop(disabled.span("read"));
        assert!(disabled.report().is_none());
        let profile = Profile::enabled("test");
        let clone = profile.clone();
        profile.add("bytes", 7);
        clone.add("bytes", 11);
        drop(profile.span("read"));
        drop(clone.span("read"));
        let report = profile.report().unwrap();
        assert_eq!(report.measurements["bytes"].units, 18);
        assert_eq!(report.measurements["read"].calls, 2);
        assert!(report.inclusive_spans);
    }
}
