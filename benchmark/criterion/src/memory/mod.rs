//! Phase-memory experiments. No subprocess elapsed time is a memory observation.
mod allocator;
mod common;
mod fixture;
#[cfg(target_os = "linux")]
mod linux;
mod process;

use criterion::{
    Criterion, SamplingMode, Throughput,
    measurement::{Measurement, ValueFormatter},
};
use std::{path::PathBuf, time::Duration};

/// Signed bytes: paired savings may legitimately be negative.
#[derive(Clone, Copy)]
struct Bytes;
struct ByteFormatter;
impl ValueFormatter for ByteFormatter {
    fn format_value(&self, value: f64) -> String {
        format!("{value:.2} B")
    }
    fn format_throughput(&self, _: &Throughput, value: f64) -> String {
        self.format_value(value)
    }
    fn scale_values(&self, _: f64, _: &mut [f64]) -> &'static str {
        "B"
    }
    fn scale_throughputs(&self, _: f64, _: &Throughput, _: &mut [f64]) -> &'static str {
        "B"
    }
    fn scale_for_machines(&self, _: &mut [f64]) -> &'static str {
        "B"
    }
}
impl Measurement for Bytes {
    type Intermediate = ();
    type Value = f64;
    fn start(&self) {}
    fn end(&self, _: ()) -> f64 {
        panic!("memory requires explicit phase counters, not start/end")
    }
    fn add(&self, a: &f64, b: &f64) -> f64 {
        a + b
    }
    fn zero(&self) -> f64 {
        0.0
    }
    fn to_f64(&self, value: &f64) -> f64 {
        *value
    }
    fn formatter(&self) -> &dyn ValueFormatter {
        &ByteFormatter
    }
}

/// Register opt-in memory experiments. Each Criterion iteration owns a complete,
/// shuffled matrix of fresh environments, preserving round pairing.
///
/// `PVISOR_MEMORY_CONFIG` is a JSON configuration (see memory README). Raw
/// observations, including Criterion warmup/calibration iterations, are retained
/// separately. Criterion statistics describe one preregistered signed byte metric;
/// CPU, readback time, PSS and complete memory curves remain distinct raw fields.
pub fn register(_: &mut Criterion) {
    let path = std::env::var_os("PVISOR_MEMORY_CONFIG").expect(
        "selected memory experiment requires PVISOR_MEMORY_CONFIG; see benchmark/memory/README.md",
    );
    let cfg: common::Config =
        common::read_json(&PathBuf::from(path)).expect("read PVISOR_MEMORY_CONFIG");
    if let Ok(entry) = std::env::var("PVISOR_BENCH_ENTRY") {
        assert!(
            entry
                .rsplit('/')
                .next()
                .map(|s| s.trim_end_matches(".json"))
                == Some(cfg.experiment.as_str()),
            "PVISOR_MEMORY_CONFIG experiment must match selected entry"
        );
    }
    let mut run = common::Run::new(cfg).expect("prepare memory evidence");
    if run.cfg.preflight {
        run.round()
            .expect("memory correctness preflight; evidence retained");
        run.finish().expect("final preflight validation");
        return;
    }
    let mut criterion = Criterion::default()
        .with_measurement(Bytes)
        .nresamples(5000);
    if let Some(path) = std::env::var_os("PVISOR_CRITERION_OUTPUT") {
        criterion = criterion.output_directory(&PathBuf::from(path));
    }
    if let Ok(name) = std::env::var("PVISOR_CRITERION_SAVE_BASELINE") {
        criterion = criterion.save_baseline(name);
    }
    if let Ok(name) = std::env::var("PVISOR_CRITERION_BASELINE") {
        criterion = criterion.retain_baseline(name, true);
    }
    let mut group = criterion.benchmark_group(format!("memory/{}", run.cfg.experiment));
    // Linear sampling would weight later samples by more expensive VM matrices.
    // Flat makes the independent unit a full paired round in every sample.
    group
        .sampling_mode(SamplingMode::Flat)
        .sample_size(10)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(1));
    let name = run.cfg.metric.clone();
    group.bench_function(name, |b| b.iter_custom(|iterations| {
        let mut sum = 0.0;
        for _ in 0..iterations {
            sum += run.round().unwrap_or_else(|error| {
                let _ = common::save(&run.cfg.output.join("failure.json"),
                    &serde_json::json!({"error":format!("{error:#}"),"completed_rounds":run.index}));
                panic!("memory experiment stopped; evidence retained: {error:#}")
            });
        }
        sum
    }));
    group.finish();
    run.finish().expect("final memory evidence validation");
    criterion.final_summary();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn memory_units_are_signed_bytes() {
        assert_eq!(Bytes.add(&-4.0, &1.0), -3.0);
        assert_eq!(Bytes.formatter().format_value(-4.0), "-4.00 B");
        let mut x = [-4.0, 5.0];
        assert_eq!(Bytes.formatter().scale_values(5.0, &mut x), "B");
        assert_eq!(x, [-4.0, 5.0]);
    }
}
