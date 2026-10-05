use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
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
        .write_all(b"one boot only")
        .unwrap();
    // Optional stage workload; the marker lives in the immutable input base.
    let stage_files = match fs::read_to_string("/stage-file-count") {
        Ok(value) => value.trim().parse::<usize>().unwrap(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => panic!("stage fixture marker: {}", error),
    };
    assert!(stage_files <= 8192);
    if stage_files != 0 {
        fs::create_dir("/stage-files").unwrap();
        for index in 0..stage_files {
            fs::write(format!("/stage-files/{index:04}"), [0x5a; 16]).unwrap();
        }
    }
    let mut data: Vec<u8> = (0..64 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    fs::create_dir_all("/dirs").unwrap();
    fs::write("/dirs/a", b"a").unwrap();
    fs::write("/dirs/b", b"b").unwrap();
    for name in ["c", "d", "e", "f"] {
        fs::write(format!("/dirs/{name}"), name.as_bytes()).unwrap();
    }
    fs::write("/open-file", b"abcdef").unwrap();
    let mut fd = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/open-file")
        .unwrap();
    fd.seek(SeekFrom::Start(2)).unwrap();
    let mut n = 0u64;
    let boot = Instant::now();
    let mut last = String::new();
    let mut directory = fs::read_dir("/dirs").unwrap();
    let first = directory.next().unwrap().unwrap().file_name();
    atomic("/ready", &format!("{}", first.to_string_lossy()));
    loop {
        n += 1;
        data[0] = (n % 251) as u8;
        atomic("/heartbeat", &n.to_string());
        if let Ok(req) = fs::read_to_string("/request") {
            if req != last {
                let start = Instant::now();
                assert!(data
                    .iter()
                    .enumerate()
                    .skip(1)
                    .all(|(i, b)| *b == (i % 251) as u8));
                assert_eq!(data[0], (n % 251) as u8);
                let next = directory.next().unwrap().unwrap().file_name();
                assert_ne!(next, first);
                let mut bytes = [0; 2];
                assert_eq!(fd.stream_position().unwrap(), 2);
                fd.read_exact(&mut bytes).unwrap();
                assert_eq!(&bytes, b"cd");
                fd.seek(SeekFrom::Start(2)).unwrap();
                assert_eq!(fs::read("/open-file").unwrap(), b"abcdef");
                for index in 0..stage_files {
                    assert_eq!(
                        fs::read(format!("/stage-files/{index:04}")).unwrap(),
                        [0x5a; 16]
                    );
                }
                atomic(
                    "/ack",
                    &format!(
                        "{} {} {:.6} {}",
                        req,
                        n,
                        start.elapsed().as_secs_f64() * 1000.,
                        boot.elapsed().as_millis()
                    ),
                );
                last = req;
            }
        }
        if File::open("/exit").is_ok() {
            println!("snapshot-guest-ok n={n}");
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
