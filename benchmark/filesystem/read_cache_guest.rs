//! Identical static native/guest payload. Read + full memcmp are inside clocks.
//! Input is a hashed deterministic 1 MiB high-entropy block repeated in files;
//! only the expected block and one read buffer are allocated, never whole WSS.
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::path::Path;
use std::time::Instant;

unsafe extern "C" {
    fn close(fd: i32) -> i32;
    fn ioctl(fd: i32, request: std::ffi::c_ulong, ...) -> i32;
}
const MIB: usize = 1 << 20;
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn close_checked(file: File) -> io::Result<()> {
    if unsafe { close(file.into_raw_fd()) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn emit(prefix: &str, value: Value) -> io::Result<()> {
    println!("{prefix} {value}");
    io::stdout().flush()
}
fn checkpoint(marker: &Option<File>, token: u64) -> io::Result<()> {
    if let Some(file) = marker {
        let value = token.to_le();
        if unsafe { ioctl(file.as_raw_fd(), 0x4008_76f1, &value as *const u64) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
fn run() -> io::Result<()> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err(invalid("expected PLAN.json INPUT_ROOT ROUNDS"));
    }
    let plan: Value =
        serde_json::from_slice(&fs::read(&args[0])?).map_err(|_| invalid("invalid plan JSON"))?;
    let cells = plan
        .get("cells")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("missing cells"))?;
    if plan["schema"] != 1 || plan["passes"] != 5 || cells.is_empty() || cells.len() > 32 {
        return Err(invalid("unsupported plan schema/passes/cell count"));
    }
    let rounds = args[2]
        .to_str()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|r| (1..=16).contains(r))
        .ok_or_else(|| invalid("rounds must be 1..16"))?;
    let root = Path::new(&args[1]).canonicalize()?;
    let expected = fs::read(
        root.parent()
            .ok_or_else(|| invalid("input root needs parent"))?
            .join("pattern.bin"),
    )?;
    if expected.len() != MIB {
        return Err(invalid("pattern must be exactly 1 MiB"));
    }
    let mut ids = BTreeSet::new();
    for cell in cells {
        let id = cell["id"]
            .as_str()
            .ok_or_else(|| invalid("missing cell id"))?;
        let name = cell["file"]
            .as_str()
            .ok_or_else(|| invalid("missing file"))?;
        let size = cell["bytes"]
            .as_u64()
            .ok_or_else(|| invalid("invalid byte count"))?;
        if id.is_empty()
            || id.len() > 64
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || !ids.insert(id)
            || name.is_empty()
            || name.contains(['/', '\\'])
            || name == "."
            || name == ".."
            || size == 0
            || size > 1024 * MIB as u64
            || size % MIB as u64 != 0
        {
            return Err(invalid("invalid/duplicate cell or unsafe input file"));
        }
    }
    let marker = match std::env::var_os("READ_CACHE_PROFILE") {
        None => None,
        Some(value) if value == "0" => None,
        Some(value) if value == "1" => Some(File::open("/")?),
        Some(_) => return Err(invalid("READ_CACHE_PROFILE must be 0 or 1")),
    };
    let mut buffer = vec![0; MIB];
    emit(
        "READ_CACHE_READY",
        json!({"schema":1,"cells":cells.len(),"rounds":rounds,"passes":5,
                                   "buffer_bytes":MIB,"verification":"inline_memcmp_repeated_1m"}),
    )?;
    for round in 1..=rounds {
        for (index, cell) in cells.iter().enumerate() {
            let size = cell["bytes"].as_u64().unwrap();
            let path = root.join(cell["file"].as_str().unwrap());
            let mut results = Vec::new();
            let full_start = Instant::now();
            for pass in 1..=5 {
                let token = (2 * (((round - 1) * cells.len() + index) * 5 + pass - 1) + 1) as u64;
                checkpoint(&marker, token)?;
                let started = Instant::now();
                let mut file = File::open(&path)?;
                let opened = Instant::now();
                for _ in 0..size / MIB as u64 {
                    file.read_exact(&mut buffer)?;
                    if buffer != expected {
                        return Err(invalid("read bytes differ from expected pattern"));
                    }
                }
                if file.read(&mut [0u8; 1])? != 0 {
                    return Err(invalid("input has unexpected trailing bytes"));
                }
                let read = Instant::now();
                close_checked(file)?;
                let closed = Instant::now();
                checkpoint(&marker, token + 1)?;
                results.push((
                    pass,
                    opened.duration_since(started).as_nanos(),
                    read.duration_since(opened).as_nanos(),
                    closed.duration_since(read).as_nanos(),
                    closed.duration_since(started).as_nanos(),
                    token,
                ));
            }
            let full_ns = full_start.elapsed().as_nanos();
            let passes = results.iter().map(|&(pass, open_ns, read_check_ns, close_ns, total_ns, token)|
                json!({"pass":pass,"bytes":size,"open_ns":open_ns,"read_check_ns":read_check_ns,
                       "close_ns":close_ns,"total_ns":total_ns,"before_token":token,
                       "after_token":token+1,"correctness":true})).collect::<Vec<_>>();
            emit(
                "READ_CACHE_RESULT",
                json!({"schema":1,"id":cell["id"],"round":round,
                "bytes":size,"passes":passes,"total_ns":full_ns,
                "pass_ns_sum":results.iter().map(|p| p.4).sum::<u128>(),"correctness":true}),
            )?;
        }
    }
    emit(
        "READ_CACHE_DONE",
        json!({"schema":1,"rows":cells.len()*rounds,"passes":cells.len()*rounds*5,
                                  "verified":true}),
    )
}
fn main() {
    if let Err(error) = run() {
        eprintln!("READ_CACHE_ERROR {error}");
        std::process::exit(1);
    }
}
