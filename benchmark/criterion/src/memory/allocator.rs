//! B-ALLOCATOR-RECLAIM-ENG native PID-1 protocol and macOS process accounting.
use super::common::*;
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::{BufWriter, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn send(stream: &mut UnixStream, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= 1048576, "oversized outgoing frame");
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}
fn receive(stream: &mut UnixStream) -> Result<Value> {
    let mut size = [0; 4];
    stream.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    ensure!(size <= 1048576, "oversized guest frame");
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes)?;
    let frame: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        !["Failure", "Error"].contains(&frame["kind"].as_str().unwrap_or("")),
        "guest failure {frame}"
    );
    Ok(frame)
}
fn connect(endpoint: &Path) -> Result<UnixStream> {
    let stream = UnixStream::connect(endpoint)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    Ok(stream)
}
fn request(endpoint: &Path, frame: Value) -> Result<Value> {
    let mut stream = connect(endpoint)?;
    send(&mut stream, &frame)?;
    receive(&mut stream)
}
fn guest_file(endpoint: &Path, path: &str) -> Result<String> {
    let mut stream = connect(endpoint)?;
    send(
        &mut stream,
        &json!({"operation":"File","request":{"action":"Read","path":path}}),
    )?;
    ensure!(
        receive(&mut stream)?["kind"] == "Ready",
        "guest file not ready"
    );
    let mut bytes = Vec::new();
    loop {
        let frame = receive(&mut stream)?;
        if frame["kind"] == "Exit" {
            ensure!(frame["code"] == 0, "guest file failed");
            break;
        }
        ensure!(frame["kind"] == "Output", "unexpected file frame");
        for byte in frame["bytes"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("missing bytes"))?
        {
            let n = byte
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("invalid byte"))?;
            ensure!(n <= 255, "invalid output byte");
            bytes.push(n as u8);
        }
        ensure!(bytes.len() < 4 * 1024 * 1024, "guest file too large");
    }
    Ok(String::from_utf8(bytes)?)
}
fn counters(text: &str) -> Value {
    let mut result = serde_json::Map::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':') {
            let mut fields = value.split_whitespace();
            if let Some(value) = fields.next().and_then(|s| s.parse::<u64>().ok()) {
                result.insert(
                    key.into(),
                    json!(value * if fields.next() == Some("kB") { 1024 } else { 1 }),
                );
            }
        }
    }
    Value::Object(result)
}
fn memory(endpoint: &Path, pid: u64) -> Result<Value> {
    Ok(
        json!({"guest":counters(&guest_file(endpoint,"/proc/meminfo")?),
        "process":counters(&guest_file(endpoint,&format!("/proc/{pid}/status"))?)}),
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn usage(pid: u32) -> Result<Value> {
    #[repr(C)]
    #[derive(Default)]
    struct Usage {
        uuid: [u8; 16],
        counters: [u64; 10],
    }
    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, value: *mut Usage) -> i32;
    }
    let mut value = Usage::default();
    const _: () = assert!(std::mem::size_of::<Usage>() == 96);
    ensure!(
        unsafe { proc_pid_rusage(pid as i32, 0, &mut value) } == 0,
        "proc_pid_rusage: {}",
        std::io::Error::last_os_error()
    );
    let names = [
        "user_time",
        "system_time",
        "pkg_idle_wkups",
        "interrupt_wkups",
        "pageins",
        "wired_size",
        "resident_size",
        "phys_footprint",
        "proc_start_abstime",
        "proc_exit_abstime",
    ];
    Ok(Value::Object(
        names
            .into_iter()
            .zip(value.counters)
            .map(|(k, v)| (k.into(), json!(v)))
            .collect(),
    ))
}
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn usage(_: u32) -> Result<Value> {
    bail!("allocator_reclaim requires Apple Silicon macOS proc_pid_rusage")
}

fn guard(owned: Option<u32>) -> Result<()> {
    let text = cmd(&["ps", "-axo", "pid=,comm="], 5)?;
    for line in text.lines() {
        let Some((pid, name)) = line.trim().split_once(char::is_whitespace) else {
            continue;
        };
        let pid = pid.parse::<u32>()?;
        let name = Path::new(name.trim())
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        ensure!(
            Some(pid) == owned
                || ![
                    "pvisor",
                    "vmm",
                    "firecracker",
                    "qemu-system-aarch64",
                    "qemu-system-x86_64",
                    "cargo",
                    "rustc",
                    "zig",
                    "cargo-nextest"
                ]
                .contains(&name),
            "foreign VM/build interference: {pid} {name}"
        );
    }
    Ok(())
}
fn observe(child: &mut OwnedChild, seconds: u64, path: &Path) -> Result<Vec<Value>> {
    let mut file = BufWriter::new(File::create(path)?);
    let mut rows = Vec::new();
    let start = Instant::now();
    let mut next_guard = Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        ensure!(child.running()?, "VMM exited during observation");
        if Instant::now() >= next_guard {
            guard(Some(child.0.id()))?;
            next_guard = Instant::now() + Duration::from_secs(1);
        }
        let mut row = usage(child.0.id())?;
        row["time"] = json!(start.elapsed().as_secs_f64());
        if let Some(first) = rows.first() {
            let first: &Value = first;
            ensure!(
                row["proc_start_abstime"] == first["proc_start_abstime"],
                "VMM identity changed"
            );
        }
        serde_json::to_writer(&mut file, &row)?;
        file.write_all(b"\n")?;
        file.flush()?;
        rows.push(row);
        thread::sleep(Duration::from_millis(250));
    }
    Ok(rows)
}
fn summary(rows: &[Value]) -> Result<Value> {
    let end = rows
        .last()
        .and_then(|r| r["time"].as_f64())
        .ok_or_else(|| anyhow::anyhow!("no observations"))?;
    ensure!(
        end - rows[0]["time"].as_f64().unwrap_or(end) >= 4.,
        "short phase"
    );
    let tail = rows
        .iter()
        .filter(|r| r["time"].as_f64().unwrap_or(0.) >= end - 2.)
        .collect::<Vec<_>>();
    ensure!(
        tail.len() >= 6,
        "insufficient final two-second observations"
    );
    let mut result = serde_json::Map::new();
    for key in ["resident_size", "phys_footprint", "wired_size"] {
        result.insert(
            key.into(),
            json!(median(
                tail.iter()
                    .map(|r| r[key].as_u64().unwrap() as f64)
                    .collect()
            )?),
        );
    }
    Ok(Value::Object(result))
}
fn trial(cfg: &Config, root: &Path, mib: u64, trim: bool) -> Result<Value> {
    guard(None)?;
    fs::create_dir(root)?;
    let input = &cfg.inputs;
    let guest = root.join("root");
    command(
        &[
            "cp".into(),
            "-a".into(),
            input.join("root").display().to_string(),
            guest.display().to_string(),
        ],
        30,
    )?;
    let endpoint = root.join("c.sock");
    let args = [
        guest,
        input.join("firmware"),
        input.join("payload"),
        input.join("reclaim.so"),
        endpoint.clone(),
    ];
    save(
        &root.join("command.json"),
        &json!({"program":input.join("vmm"),"args":args}),
    )?;
    let mut child = OwnedChild::spawn(
        Command::new(input.join("vmm"))
            .args(&args)
            .stdin(Stdio::null())
            .stdout(File::create(root.join("vmm.stdout"))?)
            .stderr(File::create(root.join("vmm.stderr"))?),
    )?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if request(&endpoint, json!({"operation":"Ping"})).is_ok_and(|v| v["kind"] == "Ready") {
            break;
        }
        ensure!(
            child.running()? && Instant::now() < deadline,
            "PID-1 adapter readiness deadline"
        );
        thread::sleep(Duration::from_millis(100));
    }
    let mut stream = connect(&endpoint)?;
    send(
        &mut stream,
        &json!({"operation":"Start","launch":{"argv":["/payload",mib.to_string()],"cwd":"/","user":null,"tty":null}}),
    )?;
    let started = receive(&mut stream)?;
    ensure!(started["kind"] == "Started", "unexpected Start reply");
    let pid = started["pid"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("missing PID"))?;
    let generation = started["generation"].clone();
    let mut reader = stream.try_clone()?;
    let (tx, rx) = mpsc::sync_channel(64);
    let frame_path = root.join("guest.frames.jsonl");
    let handle = thread::spawn(move || -> Result<()> {
        let result = (|| {
            let mut frames = File::create(frame_path)?;
            let mut pending = Vec::new();
            loop {
                let frame = receive(&mut reader)?;
                serde_json::to_writer(&mut frames, &frame)?;
                frames.write_all(b"\n")?;
                frames.flush()?;
                if frame["kind"] == "Exit" {
                    tx.send(frame)?;
                    return Ok(());
                }
                ensure!(
                    frame["kind"] == "Output" && frame["stream"] == 1,
                    "invalid payload output frame"
                );
                for byte in frame["bytes"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("missing output bytes"))?
                {
                    let n = byte
                        .as_u64()
                        .ok_or_else(|| anyhow::anyhow!("invalid byte"))?;
                    ensure!(n < 256, "invalid byte");
                    pending.push(n as u8);
                }
                ensure!(pending.len() < 1048576, "payload line too long");
                while let Some(n) = pending.iter().position(|b| *b == b'\n') {
                    let line = pending.drain(..=n).collect::<Vec<_>>();
                    tx.send(serde_json::from_slice::<Value>(&line)?)?;
                }
            }
        })();
        if let Err(ref error) = result {
            let _ = tx.send(json!({"reader_error":error.to_string()}));
        }
        result
    });
    let phase = |name: &str| -> Result<()> {
        let event = rx.recv_timeout(Duration::from_secs(30))?;
        ensure!(
            event["phase"] == name && event["pid"] == pid,
            "invalid payload phase {name}: {event}"
        );
        Ok(())
    };
    let input_byte =
        |stream: &mut UnixStream, byte: u8| send(stream, &json!({"input":"Data","bytes":[byte]}));
    let reclaim = |enabled: bool| -> Result<Value> {
        let frame = request(
            &endpoint,
            json!({"operation":"ReclaimAllocator","pid":pid,"generation":generation,"trim":enabled}),
        )?;
        ensure!(
            frame["kind"] == "Value",
            "invalid allocator response {frame}"
        );
        Ok(frame["value"].clone())
    };
    let result = (|| {
        phase("allocated")?;
        let inspection = reclaim(false)?;
        let allocated = memory(&endpoint, pid)?;
        input_byte(&mut stream, b'F')?;
        phase("freed")?;
        let freed = memory(&endpoint, pid)?;
        ensure!(
            freed["process"]["RssAnon"].as_u64().unwrap_or(0) as f64 >= mib as f64 * 1048576. * 0.8,
            "payload failed to retain freed heap"
        );
        let before = summary(&observe(&mut child, 5, &root.join("before.jsonl"))?)?;
        let reply = reclaim(trim)?;
        let after = summary(&observe(&mut child, 15, &root.join("after.jsonl"))?)?;
        let settled = memory(&endpoint, pid)?;
        input_byte(&mut stream, b'R')?;
        phase("refilled-verified")?;
        let refill = usage(child.0.id())?;
        let live_reply = reclaim(trim)?;
        observe(&mut child, 3, &root.join("live-reuse.jsonl"))?;
        input_byte(&mut stream, b'Q')?;
        phase("verified-exit")?;
        ensure!(
            rx.recv_timeout(Duration::from_secs(10))? == json!({"kind":"Exit","code":0}),
            "payload exit failed"
        );
        ensure!(
            request(&endpoint, json!({"operation":"Stop","signal":15}))?["kind"] == "Ready",
            "Stop failed"
        );
        ensure!(child.wait(15)?.code() == Some(143), "unexpected VMM exit");
        let mut drop = serde_json::Map::new();
        for key in ["phys_footprint", "resident_size", "wired_size"] {
            drop.insert(
                key.into(),
                json!(before[key].as_f64().unwrap() - after[key].as_f64().unwrap()),
            );
        }
        Ok(
            json!({"mib":mib,"trim":trim,"started":started,"inspection":inspection,"allocated":allocated,
            "freed":freed,"reply":reply,"settled":settled,"refill_host":refill,"live_reply":live_reply,
            "before":before,"after":after,"drop":drop,"payload_exit":0,"vm_exit":143}),
        )
    })();
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let joined = handle
        .join()
        .map_err(|_| anyhow::anyhow!("guest reader panicked"))?;
    let value: Value = result?;
    joined?;
    save(&root.join("result.json"), &value)?;
    Ok(value)
}
pub fn round(cfg: &Config, root: &Path, index: u64) -> Result<f64> {
    ensure!(
        cfg!(all(target_os = "macos", target_arch = "aarch64")),
        "allocator requires Apple Silicon macOS"
    );
    ensure!(
        std::env::var("PVISOR_GUEST_RECLAIM").as_deref() == Ok("1"),
        "explicit PVISOR_GUEST_RECLAIM=1 required"
    );
    let (mib, key) = match cfg.metric.as_str() {
        "64-phys-footprint-extra-drop" => (64, "phys_footprint"),
        "256-phys-footprint-extra-drop" => (256, "phys_footprint"),
        "64-resident-extra-drop" => (64, "resident_size"),
        "256-resident-extra-drop" => (256, "resident_size"),
        _ => bail!("unknown allocator byte metric"),
    };
    let mut order = vec![(64, false), (64, true), (256, false), (256, true)];
    Rng(cfg.seed.wrapping_add(index)).shuffle(&mut order);
    save(&root.join("order.json"), &order)?;
    let mut rows = Vec::new();
    for (size, trim) in order {
        rows.push(trial(
            cfg,
            &root.join(format!("{size}-{}", if trim { "trim" } else { "inspect" })),
            size,
            trim,
        )?);
        save(&root.join("samples.json"), &rows)?;
    }
    let value = |trim: bool| {
        rows.iter()
            .find(|r| r["mib"] == mib && r["trim"] == trim)
            .unwrap()["drop"][key]
            .as_f64()
            .unwrap()
    };
    Ok(value(true) - value(false))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_frames_and_oversized_frames_fail() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        a.write_all(&1048577u32.to_be_bytes()).unwrap();
        assert!(receive(&mut b).is_err());
    }
    #[test]
    fn counters_preserve_byte_units() {
        assert_eq!(
            counters("RssAnon: 123 kB\nThreads: 4\n"),
            json!({"RssAnon":125952,"Threads":4})
        );
    }
    #[test]
    fn phase_summary_rejects_short_windows() {
        assert!(summary(&[json!({"time":0}), json!({"time":3})]).is_err());
    }
}
