//! Small real-Criterion checks. These are tool fixtures, not product results.
use criterion::{
    Criterion, SamplingMode, Throughput,
    measurement::{Measurement, ValueFormatter},
};
use std::{path::Path, time::Duration};

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
        panic!("use explicit byte observations")
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

pub(crate) fn bytes() {
    let mut c = Criterion::default()
        .with_measurement(Bytes)
        .sample_size(10)
        .warm_up_time(Duration::from_millis(10))
        .measurement_time(Duration::from_millis(20))
        .with_output_color(false);
    if let Some(path) = std::env::var_os("PVISOR_CRITERION_OUTPUT") {
        c = c.output_directory(Path::new(&path));
    }
    if let Ok(name) = std::env::var("PVISOR_CRITERION_SAVE_BASELINE") {
        c = c.save_baseline(name);
    }
    if let Ok(name) = std::env::var("PVISOR_CRITERION_BASELINE") {
        c = c.retain_baseline(name, true);
    }
    let mut group = c.benchmark_group("harness");
    group.sampling_mode(SamplingMode::Flat);
    group.bench_function("bytes-per-operation", |b| {
        b.iter_custom(|iters| {
            // One operation produces exactly 37 bytes, irrespective of batch size.
            let mut total = 0.0;
            for _ in 0..iters {
                std::thread::sleep(Duration::from_micros(20)); // excluded setup
                total += 37.0;
            }
            total
        })
    });
    group.finish();
    c.final_summary();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn byte_sums_normalize_to_bytes_per_operation_not_nanoseconds() {
        let mut total = Bytes.zero();
        for _ in 0..7 {
            total = Bytes.add(&total, &37.0);
        }
        assert_eq!(Bytes.to_f64(&total) / 7.0, 37.0);
        assert_eq!(Bytes.formatter().format_value(total / 7.0), "37.00 B");
        let mut values = [37.0, 74.0];
        assert_eq!(Bytes.formatter().scale_for_machines(&mut values), "B");
        assert_eq!(values, [37.0, 74.0]);
    }
}
