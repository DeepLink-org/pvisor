//! Static Linux workload shared by KVM, HVF and WHP CLI contracts.
use std::{
    fs,
    io::Write,
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let cpus: usize = args[1].parse()?;
    let seed: u64 = args[2].parse()?;
    let code: i32 = args[3].parse()?;
    assert_eq!(fs::read("/readonly/fixture")?, b"immutable");
    assert!(fs::write("/readonly/fixture", b"changed").is_err());
    assert_eq!(
        fs::read_to_string("/sys/devices/system/cpu/online")?.trim(),
        if cpus == 1 { "0" } else { "0-1" }
    );
    let start = Instant::now();
    std::thread::sleep(Duration::from_millis(100));
    assert!(start.elapsed() >= Duration::from_millis(100));
    assert!(start.elapsed() < Duration::from_secs(10));
    fs::create_dir_all("/work")?;
    let threads: Vec<_> = (0..8u64)
        .map(|worker| {
            std::thread::spawn(move || {
                let mut state = seed ^ worker;
                for iteration in 0..32 {
                    state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let data = vec![(state >> 32) as u8; 4096 + (state as usize % 4096)];
                    let path = format!("/work/{worker}-{iteration}");
                    let temp = format!("{path}.tmp");
                    let mut file = fs::File::create(&temp).unwrap();
                    file.write_all(&data).unwrap();
                    file.sync_all().unwrap();
                    drop(file);
                    fs::rename(&temp, &path).unwrap();
                    assert_eq!(fs::read(&path).unwrap(), data);
                    fs::remove_file(path).unwrap();
                }
                // Touch memory rather than merely reserving address space.
                let mut memory = vec![worker as u8; 2 * 1024 * 1024];
                for byte in &mut memory {
                    *byte = byte.wrapping_add(1);
                }
                assert!(memory.iter().all(|byte| *byte == worker as u8 + 1));
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(fs::read_dir("/work")?.count(), 0);
    fs::create_dir_all("/work/before/nested")?;
    fs::write("/work/before/nested/file", b"data")?;
    assert_eq!(fs::metadata("/work/before/nested/file")?.len(), 4);
    fs::rename("/work/before", "/work/after")?;
    assert_eq!(fs::read("/work/after/nested/file")?, b"data");
    fs::remove_dir_all("/work/after")?;
    let result = format!("vm-contract-v1 cpus={cpus} seed={seed} exit={code}\n");
    let mut file = fs::File::create("/result")?;
    file.write_all(result.as_bytes())?;
    file.sync_all()?;
    drop(file);
    println!("{result}");
    std::process::exit(code);
}
