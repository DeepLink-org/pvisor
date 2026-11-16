//! Static Rust DAX subject. Build for x86_64-unknown-linux-musl.
//! Validation and prefaulting are outside each phase timer.
#[cfg(target_os = "linux")]
mod linux {
    use std::{
        collections::BTreeMap,
        fs::{self, File},
        io,
        os::{
            fd::{AsRawFd, IntoRawFd},
            unix::fs::FileExt,
        },
        path::Path,
        time::Instant,
    };
    const BLOCK: usize = 1 << 20;
    const LARGE: usize = 64 << 20;
    const SMALL: usize = 64 << 10;
    fn ensure(ok: bool, what: &str) -> io::Result<()> {
        if ok {
            Ok(())
        } else {
            Err(io::Error::other(what))
        }
    }
    fn close(file: File) -> io::Result<()> {
        ensure(unsafe { libc::close(file.into_raw_fd()) } == 0, "close")
    }
    fn pattern(seed: u32) -> Vec<u8> {
        let mut x = seed;
        let mut result = Vec::with_capacity(BLOCK);
        for _ in 0..BLOCK / 4 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            result.extend_from_slice(&x.to_le_bytes());
        }
        result
    }
    fn verify(bytes: &[u8], seed: u32) -> io::Result<()> {
        let expected = pattern(seed);
        ensure(
            bytes
                .chunks(BLOCK)
                .all(|chunk| chunk == &expected[..chunk.len()]),
            "content",
        )
    }
    fn buffer(length: usize) -> Vec<u8> {
        let mut bytes = vec![0; length];
        for i in (0..length).step_by(4096) {
            unsafe { std::ptr::write_volatile(bytes.as_mut_ptr().add(i), 0xa5) };
        }
        unsafe { std::ptr::write_volatile(bytes.as_mut_ptr().add(length - 1), 0xa5) };
        std::hint::black_box(&mut bytes);
        bytes
    }
    fn open(name: &str) -> io::Result<File> {
        File::open(Path::new("/tmp/data").join(name))
    }
    fn read(file: &File, bytes: &mut [u8], offset: u64) -> io::Result<()> {
        for (index, chunk) in bytes.chunks_mut(SMALL).enumerate() {
            ensure(
                file.read_at(chunk, offset + (index * SMALL) as u64)? == chunk.len(),
                "short pread",
            )?;
        }
        Ok(())
    }
    fn parallel(file: &File, bytes: &mut [u8]) -> io::Result<()> {
        let (a, b) = bytes.split_at_mut(LARGE / 2);
        std::thread::scope(|s| {
            let a = s.spawn(|| read(file, a, 0));
            let b = s.spawn(|| read(file, b, (LARGE / 2) as u64));
            a.join().map_err(|_| io::Error::other("reader panic"))??;
            b.join().map_err(|_| io::Error::other("reader panic"))?
        })
    }
    fn metadata() -> io::Result<()> {
        let mut size = 0;
        for i in 0..2048 {
            let m = fs::metadata(format!("/tmp/data/meta/d{:02}/f{i:04}", i / 64))?;
            ensure(m.is_file(), "metadata kind")?;
            size += m.len();
        }
        ensure(size == 2048 * 256, "metadata size")
    }
    fn mount(
        source: &std::ffi::CStr,
        target: &std::ffi::CStr,
        kind: &std::ffi::CStr,
        flags: libc::c_ulong,
        options: &std::ffi::CStr,
    ) -> io::Result<()> {
        ensure(
            unsafe {
                libc::mount(
                    source.as_ptr(),
                    target.as_ptr(),
                    kind.as_ptr(),
                    flags,
                    options.as_ptr().cast(),
                )
            } == 0,
            "mount",
        )
    }
    fn metric(
        metrics: &mut BTreeMap<String, u128>,
        totals: &mut BTreeMap<String, u128>,
        key: String,
        start: Instant,
        repetitions: u128,
    ) {
        let elapsed = start.elapsed().as_nanos();
        metrics.insert(key.clone(), elapsed / repetitions);
        totals.insert(key, elapsed);
    }
    pub fn main() -> Result<(), Box<dyn std::error::Error>> {
        let mode = std::env::args().nth(1).ok_or("mode")?;
        ensure(mode == "off" || mode == "on", "mode")?;
        let dax = mode == "on";
        mount(c"tmpfs", c"/tmp", c"tmpfs", 0, c"size=16m")?;
        fs::create_dir("/tmp/data")?;
        mount(
            c"pvisor-dax",
            c"/tmp/data",
            c"virtiofs",
            libc::MS_RDONLY,
            if dax { c"dax=inode" } else { c"dax=never" },
        )?;
        let mut stat = unsafe { std::mem::zeroed::<libc::statx>() };
        ensure(
            unsafe {
                libc::statx(
                    libc::AT_FDCWD,
                    c"/tmp/data/sequential".as_ptr(),
                    0,
                    libc::STATX_BASIC_STATS,
                    &mut stat,
                )
            } == 0,
            "statx",
        )?;
        const ATTR_DAX: u64 = 0x00200000;
        ensure(
            stat.stx_attributes_mask & ATTR_DAX != 0
                && (stat.stx_attributes & ATTR_DAX != 0) == dax,
            "DAX selection",
        )?;
        let mut metrics = BTreeMap::new();
        let mut totals = BTreeMap::new();
        for phase in ["first", "repeat"] {
            let start = Instant::now();
            metadata()?;
            metric(
                &mut metrics,
                &mut totals,
                format!("metadata_{phase}"),
                start,
                1,
            );
        }
        let mut bytes = buffer(128 * SMALL);
        for phase in ["first", "repeat"] {
            let start = Instant::now();
            for i in 0..128 {
                let f = open(&format!("small/f{i:03}"))?;
                read(&f, &mut bytes[i * SMALL..(i + 1) * SMALL], 0)?;
                close(f)?;
            }
            metric(
                &mut metrics,
                &mut totals,
                format!("small_{phase}"),
                start,
                1,
            );
            for i in 0..128 {
                verify(&bytes[i * SMALL..(i + 1) * SMALL], 1000 + i as u32)?;
            }
        }
        drop(bytes);
        let file = open("sequential")?;
        let mut bytes = buffer(LARGE);
        for (phase, repetitions) in [("first", 1), ("repeat", 4)] {
            let start = Instant::now();
            for _ in 0..repetitions {
                read(&file, &mut bytes, 0)?;
            }
            metric(
                &mut metrics,
                &mut totals,
                format!("read_{phase}"),
                start,
                repetitions,
            );
            verify(&bytes, 101)?;
        }
        close(file)?;
        drop(bytes);
        let file = open("mapped")?;
        let start = Instant::now();
        let mapped = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                LARGE,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        ensure(mapped != libc::MAP_FAILED, "mmap")?;
        let scan = || -> u64 {
            (0..LARGE)
                .step_by(4096)
                .map(|i| unsafe { std::ptr::read_volatile(mapped.cast::<u8>().add(i)) } as u64)
                .sum()
        };
        let sum = scan();
        metric(&mut metrics, &mut totals, "mmap_first".into(), start, 1);
        verify(
            unsafe { std::slice::from_raw_parts(mapped.cast::<u8>(), LARGE) },
            202,
        )?;
        let expected = pattern(202);
        let expected_sum: u64 = (0..LARGE)
            .step_by(4096)
            .map(|i| expected[i % BLOCK] as u64)
            .sum();
        ensure(sum == expected_sum, "mmap first checksum")?;
        let start = Instant::now();
        let repeated: u64 = (0..16).map(|_| scan()).sum();
        metric(&mut metrics, &mut totals, "mmap_repeat".into(), start, 16);
        ensure(repeated == 16 * expected_sum, "mmap repeat checksum")?;
        ensure(unsafe { libc::munmap(mapped, LARGE) } == 0, "unmap")?;
        close(file)?;
        let file = open("parallel")?;
        let mut bytes = buffer(LARGE);
        for (phase, repetitions) in [("first", 1), ("repeat", 4)] {
            let start = Instant::now();
            for _ in 0..repetitions {
                parallel(&file, &mut bytes)?;
            }
            metric(
                &mut metrics,
                &mut totals,
                format!("parallel_{phase}"),
                start,
                repetitions,
            );
            verify(&bytes, 303)?;
        }
        close(file)?;
        ensure(
            unsafe { libc::umount(c"/tmp/data".as_ptr()) } == 0,
            "unmount",
        )?;
        let mut kernel = unsafe { std::mem::zeroed::<libc::utsname>() };
        ensure(unsafe { libc::uname(&mut kernel) } == 0, "uname")?;
        let release =
            unsafe { std::ffi::CStr::from_ptr(kernel.release.as_ptr()) }.to_string_lossy();
        println!(
            "DAX_BENCH_RESULT {}",
            serde_json::json!({"mode":mode,"dax":dax,"metrics_ns":metrics,
        "subject_language":"rust","phase_totals_ns":totals,"guest_kernel":release,
        "verified_bytes":3*LARGE+128*SMALL,"metadata_files":2048})
        );
        Ok(())
    }
}
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    linux::main()
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("dax-guest requires Linux virtiofs/statx");
    std::process::exit(2);
}
