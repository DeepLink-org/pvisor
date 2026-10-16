//! Standalone Linux/musl workload for B-FS-ENG; phase checks precede PASS.
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
unsafe extern "C" {
    fn mount(
        source: *const std::ffi::c_char,
        target: *const std::ffi::c_char,
        kind: *const std::ffi::c_char,
        flags: usize,
        data: *const std::ffi::c_void,
    ) -> i32;
}
const FILES: usize = 2048;
const OPENS: usize = 1000;
fn paths(root: &Path, group: &str, deep: bool) -> Vec<PathBuf> {
    (0..FILES)
        .map(|i| {
            let mut dir = root.join(group).join(format!("d{:02}", i / 64));
            if deep {
                for level in 0..7 {
                    dir.push(format!("l{level}"));
                }
            }
            dir.join(format!("f{i:04}"))
        })
        .collect()
}
fn timed<T>(name: &str, call: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let value = call();
    println!(
        "CACHE_RESULT {{\"phase\":\"{name}\",\"ms\":{:.6}}}",
        start.elapsed().as_secs_f64() * 1000.
    );
    value
}
fn stats(paths: &[PathBuf]) {
    for path in paths {
        assert_eq!(fs::metadata(path).unwrap().len(), 8);
    }
}
fn scan(root: &Path) {
    let mut dirs = 0;
    let mut files = 0;
    for dir in fs::read_dir(root.join("scan")).unwrap() {
        dirs += 1;
        for entry in fs::read_dir(dir.unwrap().path()).unwrap() {
            assert_eq!(entry.unwrap().metadata().unwrap().len(), 8);
            files += 1;
        }
    }
    assert_eq!((dirs, files), (32, FILES));
}
fn read_all(paths: &[PathBuf]) -> Vec<Vec<u8>> {
    paths.iter().map(|path| fs::read(path).unwrap()).collect()
}
fn verify_read(bytes: Vec<Vec<u8>>) {
    assert_eq!(bytes.len(), FILES);
    for (i, data) in bytes.iter().enumerate() {
        assert_eq!(data.len(), 32768);
        assert!(data.iter().all(|b| *b == (i % 251) as u8));
    }
}
fn open_stat(path: &Path) {
    for _ in 0..OPENS {
        assert_eq!(File::open(path).unwrap().metadata().unwrap().len(), 8);
    }
}
fn open_read(path: &Path) {
    let mut buffer = [0; 4096];
    for _ in 0..OPENS {
        File::open(path).unwrap().read_exact(&mut buffer).unwrap();
        assert!(buffer.iter().all(|b| *b == 0));
    }
}
fn wide(root: &Path, group: &str, count: usize, step: usize, start: usize) {
    for i in 0..count {
        let index = (i * step + start) % FILES;
        assert_eq!(
            fs::metadata(root.join(group).join(format!("f{index:04}")))
                .unwrap()
                .len(),
            8
        );
    }
}
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert_eq!(args.len(), 2);
    let root = Path::new(&args[0]);
    let mode = args[1].as_str();
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
    let flat = paths(root, "flat", false);
    let deep = paths(root, "deep", true);
    let reads = paths(root, "read", false);
    match mode {
        "all" => {
            timed("stat_cold", || stats(&flat));
            timed("stat_hot", || stats(&flat));
            timed("deep_cold", || stats(&deep));
            timed("deep_hot", || stats(&deep));
            timed("scan_cold", || scan(root));
            timed("scan_hot", || scan(root));
            verify_read(timed("read_cold", || read_all(&reads)));
            verify_read(timed("read_hot", || read_all(&reads)));
            fs::metadata(&flat[0]).unwrap();
            timed("open_stat_hot", || open_stat(&flat[0]));
            fs::read(&reads[0]).unwrap();
            timed("open_read_hot", || open_read(&reads[0]));
            let sparse = root.join("wide/f1024");
            timed("sparse_cold", || {
                assert_eq!(fs::metadata(&sparse).unwrap().len(), 8)
            });
            timed("sparse_hot", || {
                assert_eq!(fs::metadata(&sparse).unwrap().len(), 8)
            });
            std::thread::sleep(Duration::from_millis(1300));
            timed("stat_expired", || stats(&flat));
            timed("wide_pair", || wide(root, "wide_pair", 2, 1023, 1024));
            timed("wide_random", || wide(root, "wide_random", 32, 997, 1024));
            timed("wide_stride", || wide(root, "wide_stride", 32, 61, 0));
            timed("wide_dense", || wide(root, "wide_dense", FILES, 1, 0));
        }
        "empty" => {}
        "stat-prime" => stats(&flat),
        "stat" => {
            stats(&flat);
            stats(&flat);
        }
        "expired" => {
            stats(&flat);
            std::thread::sleep(Duration::from_millis(1300));
            stats(&flat);
        }
        "open-prime" => {
            fs::metadata(&flat[0]).unwrap();
        }
        "open" => {
            fs::metadata(&flat[0]).unwrap();
            open_stat(&flat[0]);
        }
        "read-prime" => {
            assert_eq!(fs::read(&reads[0]).unwrap(), vec![0; 32768]);
        }
        "openread" => {
            assert_eq!(fs::read(&reads[0]).unwrap(), vec![0; 32768]);
            open_read(&reads[0]);
        }
        "sparse" => {
            assert_eq!(fs::metadata(root.join("wide/f1024")).unwrap().len(), 8);
        }
        "pair" => wide(root, "wide_pair", 2, 1023, 1024),
        "random" => wide(root, "wide_random", 32, 997, 1024),
        "stride" => wide(root, "wide_stride", 32, 61, 0),
        "dense" => wide(root, "wide_dense", FILES, 1, 0),
        "unused" => {
            wide(root, "wide_dense", 64, 1, 0);
            wide(root, "wide_dense", OPENS, 0, 0);
            wide(root, "wide_dense", 2, 1, 2000);
        }
        _ => panic!("unknown mode"),
    }
    println!("CACHE_PASS {mode}");
}
