//! B-MEMORY-SCALE controlled static/library guest fixture.
//! Build for the guest architecture; same binary and bytes in every backend.
#[path = "../criterion/src/memory/fixture.rs"]
mod fixture;
#[cfg(target_os = "linux")]
mod native {
    use super::fixture;
    use sha2::{Digest, Sha256};
    use std::{
        ffi::CString,
        io::{self, BufRead, Write},
    };
    fn verify(data: &[u8], pattern: &str, id: u64, percent: usize, token: &str) {
        assert_eq!(data.len(), fixture::SIZE);
        for (index, bytes) in data.chunks_exact(4096).enumerate() {
            assert_eq!(
                bytes,
                &fixture::page(pattern, id, index, percent),
                "full-byte mismatch page {index}"
            );
        }
        println!("F4K {token} {id} {percent} {:x}", Sha256::digest(data));
        io::stdout().flush().unwrap();
    }
    fn mount(source: &str, target: &str, kind: &str, flags: libc::c_ulong, data: Option<&str>) {
        let (source, target, kind) = (
            CString::new(source).unwrap(),
            CString::new(target).unwrap(),
            CString::new(kind).unwrap(),
        );
        let data = data.map(|s| CString::new(s).unwrap());
        assert_eq!(
            unsafe {
                libc::mount(
                    source.as_ptr(),
                    target.as_ptr(),
                    kind.as_ptr(),
                    flags,
                    data.as_ref()
                        .map_or(std::ptr::null(), |s| s.as_ptr().cast()),
                )
            },
            0,
            "mount: {}",
            io::Error::last_os_error()
        );
    }
    pub fn run() {
        let args = std::env::args()
            .skip(1)
            .filter(|s| s != "--")
            .collect::<Vec<_>>();
        let native = args.first().is_some_and(|s| s == "--native");
        let offset = usize::from(native);
        let pattern = &args[offset];
        let id: u64 = args[offset + 1].parse().unwrap();
        assert!(["repeated", "random-shared", "random-unique"].contains(&pattern.as_str()));
        let mode = args.get(offset + 2).map(String::as_str);
        let mut data = Vec::new();
        let mut library = std::ptr::null_mut();
        let mut address = std::ptr::null_mut::<u8>();
        let mut mutate: Option<unsafe extern "C" fn(u64, u32) -> i32> = None;
        if let Some(mode) = mode {
            assert_eq!(pattern, "random-shared");
            assert!(["rootfs", "block", "dax", "buffered"].contains(&mode));
            let path = if ["dax", "buffered"].contains(&mode) {
                mount("tmpfs", "/tmp", "tmpfs", 0, None);
                std::fs::create_dir("/tmp/libraries").unwrap();
                mount(
                    "bench-libraries",
                    "/tmp/libraries",
                    "virtiofs",
                    1,
                    Some(if mode == "dax" {
                        "dax=inode"
                    } else {
                        "dax=never"
                    }),
                );
                "/tmp/libraries/libpayload.so"
            } else {
                "/libraries/libpayload.so"
            };
            let path = CString::new(path).unwrap();
            let mut stat: libc::statx = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { libc::statx(libc::AT_FDCWD, path.as_ptr(), 0, 0x7ff, &mut stat) },
                0
            );
            if mode == "dax" {
                assert_ne!(stat.stx_attributes_mask & 0x200000, 0);
                assert_ne!(stat.stx_attributes & 0x200000, 0);
            } else {
                assert_eq!(stat.stx_attributes & 0x200000, 0);
            }
            println!(
                "LIBRARY_MODE {mode} statx_attributes {} statx_attributes_mask {}",
                stat.stx_attributes, stat.stx_attributes_mask
            );
            io::stdout().flush().unwrap();
            unsafe {
                library = libc::dlopen(path.as_ptr(), libc::RTLD_NOW);
                assert!(!library.is_null(), "dlopen failed");
                let symbol = |name: &str| {
                    let name = CString::new(name).unwrap();
                    let value = libc::dlsym(library, name.as_ptr());
                    assert!(!value.is_null(), "required payload symbol missing");
                    value
                };
                let addr: unsafe extern "C" fn() -> *mut u8 =
                    std::mem::transmute(symbol("payload_addr"));
                let len: unsafe extern "C" fn() -> usize =
                    std::mem::transmute(symbol("payload_length"));
                assert_eq!(len(), fixture::SIZE);
                address = addr();
                assert_eq!(address as usize % 4096, 0);
                mutate = Some(std::mem::transmute::<
                    *mut libc::c_void,
                    unsafe extern "C" fn(u64, u32) -> i32,
                >(symbol("payload_mutate")));
            }
        } else {
            data.resize(fixture::SIZE, 0);
            for (index, bytes) in data.chunks_exact_mut(4096).enumerate() {
                bytes.copy_from_slice(&fixture::page(pattern, id, index, 0));
            }
        }
        let mut percent = 0;
        // Do not copy the live payload during checks: that would add 64 MiB private
        // residency and destroy the memory contract.
        let check = |data: &Vec<u8>, percent, token: &str| {
            let bytes = if address.is_null() {
                data.as_slice()
            } else {
                unsafe { std::slice::from_raw_parts(address, fixture::SIZE) }
            };
            verify(bytes, pattern, id, percent, token);
        };
        check(&data, percent, "ready");
        for line in io::stdin().lock().lines() {
            let line = line.unwrap();
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() != 3 {
                continue;
            }
            let requested: usize = fields[2].parse().unwrap();
            match fields[1] {
                "mutate" => {
                    assert!([25, 100].contains(&requested) && requested >= percent);
                    if let Some(mutate) = mutate {
                        assert_eq!(unsafe { mutate(id, requested as u32) }, 0);
                    } else {
                        for index in 0..16384 * requested / 100 {
                            data[index * 4096..(index + 1) * 4096]
                                .copy_from_slice(&fixture::page(pattern, id, index, requested));
                        }
                    }
                    percent = requested;
                }
                "read" | "exit" => assert_eq!(requested, percent),
                _ => panic!("unknown payload operation"),
            }
            check(&data, percent, fields[0]);
            if fields[1] == "exit" {
                if mode == Some("block") || (!native && mode.is_none()) {
                    loop {
                        unsafe { libc::pause() };
                    }
                }
                if !library.is_null() {
                    unsafe { libc::dlclose(library) };
                }
                return;
            }
        }
        panic!("payload input EOF before verified exit");
    }
}
#[cfg(target_os = "linux")]
fn main() {
    native::run()
}
#[cfg(not(target_os = "linux"))]
fn main() {
    panic!("memory guest fixture requires Linux")
}
