//! Deterministic private guest memory; verify full contents across execution restore.
use std::{env, thread, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};
fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100000001b3))
}
fn main() {
    let args: Vec<String> = env::args().collect();
    let kind = &args[1];
    let bytes: usize = args[2].parse().unwrap();
    let delay: u64 = args[3].parse().unwrap();
    assert!(kind == "repeated" || kind == "random");
    assert!(bytes >= 4096);
    let token = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let mut data = vec![0u8; bytes];
    let mut state = 0x123456789abcdefu64;
    for (i, value) in data.iter_mut().enumerate() {
        *value = if kind == "repeated" { (i % 251) as u8 } else {
            state ^= state << 13; state ^= state >> 7; state ^= state << 17; state as u8
        };
    }
    let before = Instant::now();
    let expected = checksum(&data);
    let warm_ms = before.elapsed().as_secs_f64() * 1000.0;
    println!("PVISOR_MEMORY_READY {{\"kind\":\"{kind}\",\"bytes\":{bytes},\"token\":\"{token}\",\"checksum\":\"{expected:016x}\",\"warm_scan_ms\":{warm_ms}}}");
    thread::sleep(Duration::from_secs(delay));
    let before = Instant::now();
    let restored = checksum(std::hint::black_box(&data));
    assert_eq!(restored, expected, "restored guest heap changed");
    let restored_ms = before.elapsed().as_secs_f64() * 1000.0;
    println!("PVISOR_MEMORY_RESULT {{\"kind\":\"{kind}\",\"bytes\":{bytes},\"token\":\"{token}\",\"checksum\":\"{restored:016x}\",\"restored_scan_ms\":{restored_ms},\"integrity\":\"passed\"}}");
}
