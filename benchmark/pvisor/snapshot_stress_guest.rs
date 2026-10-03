//! Dependency-free static KVM guest; all assertions survive snapshots in RAM.
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

fn atomic(path: &str, value: &str) {
    fs::write(format!("{path}.tmp"), value).unwrap();
    fs::rename(format!("{path}.tmp"), path).unwrap();
}

fn main() {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open("/boot-once")
        .unwrap()
        .write_all(b"must never reboot")
        .unwrap();
    let ticks = Arc::new([const { AtomicU64::new(0) }; 4]);
    for worker in 0..4u8 {
        let ticks = ticks.clone();
        thread::spawn(move || {
            let mut data = vec![worker; 4 * 1024 * 1024];
            data[..8].fill(0);
            let mut counter = 0u64;
            loop {
                assert_eq!(&data[..8], &counter.to_le_bytes());
                assert!(data[8..].iter().all(|byte| *byte == worker));
                counter += 1;
                data[..8].copy_from_slice(&counter.to_le_bytes());
                ticks[worker as usize].fetch_add(1, Ordering::Relaxed);
                thread::sleep(Duration::from_millis(20));
            }
        });
    }
    let mut data: Vec<u8> = (0..64 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let mut tag = None;
    let mut fd = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open("/open-file")
        .unwrap();
    fd.write_all(b"abcdef").unwrap();
    fd.seek(SeekFrom::Start(2)).unwrap();
    fs::create_dir("/directory").unwrap();
    for index in 0..128 {
        fs::write(format!("/directory/{index}"), index.to_string()).unwrap();
    }
    let mut directory = fs::read_dir("/directory").unwrap();
    let first = directory.next().unwrap().unwrap().file_name();
    let mut last = String::new();
    let mut n = 0u64;
    let mut last_ticks = [0; 4];
    atomic("/ready", "threads-ram-fd-directory");
    loop {
        n += 1;
        data[0] = (n % 251) as u8;
        atomic("/heartbeat", &n.to_string());
        if let Ok(request) = fs::read_to_string("/request") {
            if request != last {
                let start = Instant::now();
                for (index, byte) in data.iter().enumerate().skip(1) {
                    let expected = if index % 4096 == 1 {
                        tag.unwrap_or((index % 251) as u8)
                    } else {
                        (index % 251) as u8
                    };
                    assert_eq!(*byte, expected, "RAM offset {index}");
                }
                assert_eq!(data[0], (n % 251) as u8);
                // Mutate every page: recapture must retain COW data, including
                // faults that occurred after deleting the backing snapshot.
                let value = request.bytes().fold(0u8, u8::wrapping_add);
                for index in (1..data.len()).step_by(4096) {
                    data[index] = value;
                }
                tag = Some(value);
                assert_eq!(fd.stream_position().unwrap(), 2);
                let mut bytes = [0; 2];
                fd.read_exact(&mut bytes).unwrap();
                assert_eq!(&bytes, b"cd");
                fd.seek(SeekFrom::Start(2)).unwrap();
                assert_eq!(fs::read("/open-file").unwrap(), b"abcdef");
                let next = directory.next().unwrap().unwrap().file_name();
                assert_ne!(next, first);
                let current_ticks = std::array::from_fn(|i| ticks[i].load(Ordering::Relaxed));
                assert!(
                    current_ticks
                        .iter()
                        .zip(last_ticks)
                        .all(|(now, old)| *now > old),
                    "guest threads stopped"
                );
                last_ticks = current_ticks;
                atomic("/private", &request);
                atomic(
                    "/ack",
                    &format!(
                        "{} {} {:.6} {}",
                        request,
                        n,
                        start.elapsed().as_secs_f64() * 1000.,
                        current_ticks.iter().sum::<u64>()
                    ),
                );
                last = request;
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
}
