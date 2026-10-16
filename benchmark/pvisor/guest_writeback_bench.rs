//! B-FS-ENG payload: sequential write/close and syncfs boundaries, checked bytes.
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::fd::{AsRawFd, IntoRawFd},
    path::Path,
    process::Command,
    time::Instant,
};
unsafe extern "C" {
    fn mount(
        source: *const std::ffi::c_char,
        target: *const std::ffi::c_char,
        kind: *const std::ffi::c_char,
        flags: usize,
        data: *const std::ffi::c_void,
    ) -> i32;
    fn syncfs(fd: i32) -> i32;
    fn close(fd: i32) -> i32;
}
const CASES: [&str; 7] = [
    "defer",
    "tiny",
    "boundary",
    "pressure",
    "overwrite",
    "stream",
    "fsync",
];
fn barrier(file: &File) {
    assert_eq!(unsafe { syncfs(file.as_raw_fd()) }, 0, "syncfs failed");
}
fn closed(file: File) {
    assert_eq!(unsafe { close(file.into_raw_fd()) }, 0, "close failed");
}
fn shape(case: &str) -> (usize, usize) {
    match case {
        "defer" => (16, 256 << 10),
        "tiny" => (200, 4096),
        "boundary" => (256, 64 << 10),
        "pressure" => (24, 1 << 20),
        "overwrite" => (16, 4096),
        "stream" => (1, 8 << 20),
        "fsync" => (16, 4096),
        _ => panic!("unknown case"),
    }
}
fn pattern(case: &str, index: usize) -> u8 {
    if case == "overwrite" {
        ((index + 63) % 251) as u8
    } else {
        (index % 251) as u8
    }
}
fn run(root: &Path, case: &str, prime: bool) {
    let directory = root.join("write-output").join(case);
    fs::create_dir_all(&directory).unwrap();
    let handle = File::open(&directory).unwrap();
    let (count, size) = shape(case);
    let paths: Vec<_> = (0..count)
        .map(|i| directory.join(format!("f{i:04}")))
        .collect();
    let data: Vec<_> = (0..count).map(|i| vec![(i % 251) as u8; size]).collect();
    if case == "overwrite" {
        for (path, bytes) in paths.iter().zip(&data) {
            fs::write(path, bytes).unwrap();
        }
    }
    barrier(&handle);
    if prime {
        println!("WB_PRIME {case}");
        return;
    }
    let blocks: Vec<_> = if case == "overwrite" {
        (0..251).map(|i| vec![i as u8; 4096]).collect()
    } else {
        Vec::new()
    };
    let start = Instant::now();
    if case == "overwrite" {
        for version in 0..64 {
            for (index, path) in paths.iter().enumerate() {
                let mut file = OpenOptions::new().write(true).open(path).unwrap();
                file.write_all(&blocks[(index + version) % 251]).unwrap();
                closed(file);
            }
        }
    } else {
        for (path, bytes) in paths.iter().zip(&data) {
            let mut file = File::create(path).unwrap();
            if case == "stream" {
                for block in bytes.chunks(1024) {
                    file.write_all(block).unwrap();
                }
            } else {
                file.write_all(bytes).unwrap();
            }
            if case == "fsync" {
                file.sync_all().unwrap();
            }
            closed(file);
        }
    }
    let written = Instant::now();
    barrier(&handle);
    let synced = Instant::now();
    for (index, path) in paths.iter().enumerate() {
        let bytes = fs::read(path).unwrap();
        assert_eq!(bytes.len(), size);
        assert!(
            bytes.iter().all(|b| *b == pattern(case, index)),
            "wrong bytes in {case}/{index}"
        );
    }
    println!(
        "WB_RESULT {{\"case\":\"{case}\",\"write_ms\":{:.6},\"sync_ms\":{:.6},\"total_ms\":{:.6}}}",
        written.duration_since(start).as_secs_f64() * 1000.,
        synced.duration_since(written).as_secs_f64() * 1000.,
        synced.duration_since(start).as_secs_f64() * 1000.
    );
}
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert_eq!(args.len(), 2);
    let root = Path::new(&args[0]);
    let mode = &args[1];
    if root == Path::new("/input") {
        fs::create_dir(root).unwrap();
        assert_eq!(
            unsafe {
                mount(
                    c"cache-input".as_ptr(),
                    c"/input".as_ptr(),
                    c"virtiofs".as_ptr(),
                    0,
                    std::ptr::null(),
                )
            },
            0
        );
    }
    for entry in fs::read_dir("/sys/class/bdi").unwrap() {
        let path = entry.unwrap().path();
        let mut fields = Vec::new();
        for name in ["min_bytes", "max_ratio", "strict_limit"] {
            if let Ok(v) = fs::read_to_string(path.join(name)) {
                fields.push(format!("{name}={}", v.trim()));
            }
        }
        if !fields.is_empty() {
            println!("WB_BDI {} {}", path.display(), fields.join(" "));
        }
    }
    if mode == "all" || mode.starts_with("meta-") {
        let sub = if mode == "all" {
            "all"
        } else {
            mode.strip_prefix("meta-").unwrap()
        };
        assert!(Command::new(root.join("metadata-probe"))
            .arg(root.join("meta"))
            .arg(sub)
            .status()
            .unwrap()
            .success());
    }
    if mode == "all" {
        for case in CASES {
            run(root, case, false);
        }
    } else if CASES.contains(&mode.as_str()) {
        run(root, mode, false);
    } else if mode == "overwrite-prime" {
        run(root, "overwrite", true);
    } else {
        assert!(mode == "empty" || mode.starts_with("meta-"), "invalid mode");
    }
    println!("WB_PASS {mode}");
}
