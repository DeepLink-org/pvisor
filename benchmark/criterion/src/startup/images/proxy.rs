//! Transparent framed cache proxy. Exact bytes and per-body hashes are retained.
use super::{process, source};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const MAX: usize = 1024 * 1024;
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct Traffic {
    pub content_bytes: u64,
    pub metadata_bytes: u64,
    pub response_bytes: u64,
}
#[derive(Default)]
struct Counts {
    active: usize,
    connections: u64,
    rows: Vec<Value>,
    errors: Vec<Value>,
    raw: Vec<Raw>,
}
#[derive(Default)]
struct Raw {
    directory: PathBuf,
    request: Vec<u8>,
    response: Vec<u8>,
    body: Vec<u8>,
}
struct Capture {
    counts: Shared,
    raw: Option<Raw>,
}
impl Drop for Capture {
    fn drop(&mut self) {
        if let Some(raw) = self.raw.take() {
            self.counts.0.lock().unwrap().raw.push(raw);
        }
    }
}
type Shared = Arc<(Mutex<Counts>, Condvar)>;
pub(super) struct Proxy {
    address: String,
    counts: Shared,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}
impl Proxy {
    pub(super) fn start_tcp(upstream: &str, evidence: &Path) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?.to_string();
        Self::start(
            format!("tcp://{address}"),
            upstream,
            evidence,
            move || match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(90)))?;
                    stream.set_nodelay(true)?;
                    Ok(Some(Box::new(stream) as Box<dyn Stream>))
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
                Err(e) => Err(e.into()),
            },
        )
    }
    pub(super) fn start_unix(socket: &Path, upstream: &str, _token: &str) -> Result<Self> {
        let listener = UnixListener::bind(socket)?;
        listener.set_nonblocking(true)?;
        let evidence = socket
            .parent()
            .context("socket parent")?
            .join("proxy-frames");
        Self::start(
            format!("unix://{}", socket.display()),
            upstream,
            &evidence,
            move || match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(90)))?;
                    Ok(Some(Box::new(stream) as Box<dyn Stream>))
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
                Err(e) => Err(e.into()),
            },
        )
    }
    fn start(
        address: String,
        upstream: &str,
        evidence: &Path,
        mut accept: impl FnMut() -> Result<Option<Box<dyn Stream>>> + Send + 'static,
    ) -> Result<Self> {
        fs::create_dir(evidence)?;
        let evidence = evidence.to_owned();
        let upstream = upstream.trim_start_matches("tcp://").to_owned();
        ensure!(
            upstream.starts_with("127.0.0.1:"),
            "proxy upstream must be loopback"
        );
        let counts = Shared::default();
        let stop = Arc::new(AtomicBool::new(false));
        let c = counts.clone();
        let s = stop.clone();
        let thread = thread::spawn(move || {
            let mut workers = Vec::new();
            while !s.load(Ordering::Acquire) {
                match accept() {
                    Ok(Some(stream)) => {
                        let id = {
                            let mut c = c.0.lock().unwrap();
                            c.connections += 1;
                            c.connections
                        };
                        let c = c.clone();
                        let upstream = upstream.clone();
                        let evidence = evidence.clone();
                        let s = s.clone();
                        workers.push(thread::spawn(move || {
                            if let Err(e) = exchange(stream, &upstream, &evidence, id, &c, &s) {
                                c.0.lock()
                                    .unwrap()
                                    .errors
                                    .push(json!({"connection_id":id,"error":format!("{e:#}")}));
                            }
                        }));
                    }
                    Ok(None) => thread::sleep(Duration::from_millis(10)),
                    Err(e) => {
                        c.0.lock()
                            .unwrap()
                            .errors
                            .push(json!({"accept_error":format!("{e:#}")}));
                        break;
                    }
                }
            }
            for worker in workers {
                if worker.join().is_err() {
                    c.0.lock()
                        .unwrap()
                        .errors
                        .push(json!({"error":"proxy thread panicked"}));
                }
            }
        });
        Ok(Self {
            address,
            counts,
            stop,
            thread: Some(thread),
        })
    }
    pub(super) fn address(&self) -> String {
        self.address.clone()
    }
    fn snapshot(&self) -> Result<(Vec<Value>, Vec<Value>)> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut counts = self.counts.0.lock().unwrap();
        while counts.active > 0 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(), "proxy did not drain");
            counts = self.counts.1.wait_timeout(counts, remaining).unwrap().0;
        }
        Ok((counts.rows.clone(), counts.errors.clone()))
    }
    pub(super) fn cursor(&self) -> Result<usize> {
        let (rows, errors) = self.snapshot()?;
        ensure!(errors.is_empty(), "proxy previously failed");
        Ok(rows.len())
    }
    pub(super) fn observation(&self, cursor: usize, evidence: &Path) -> Result<Traffic> {
        let (rows, errors) = self.snapshot()?;
        ensure!(cursor <= rows.len(), "invalid proxy cursor");
        self.flush_raw()?;
        let rows = &rows[cursor..];
        process::save(
            &evidence.join("requests.json"),
            &json!({"rows":rows,"errors":errors}),
        )?;
        ensure!(errors.is_empty(), "cache counting/forwarding proxy failed");
        let mut traffic = Traffic::default();
        for row in rows {
            traffic.content_bytes += row["content_bytes"].as_u64().unwrap();
            traffic.metadata_bytes += row["metadata_bytes"].as_u64().unwrap();
            traffic.response_bytes += row["response_bytes"].as_u64().unwrap();
        }
        Ok(traffic)
    }
    pub(super) fn stop(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("proxy accept thread panicked"))?;
        }
        let (_, errors) = self.snapshot()?;
        self.flush_raw()?;
        ensure!(errors.is_empty(), "proxy errors at teardown");
        Ok(())
    }
    fn flush_raw(&self) -> Result<()> {
        // Called only outside measured process intervals (or at teardown).
        // Requests retain exact bytes in memory until then, so disk evidence
        // writes cannot delay the application's cache responses.
        let mut counts = self.counts.0.lock().unwrap();
        for raw in &counts.raw {
            fs::create_dir_all(&raw.directory)?;
            fs::write(raw.directory.join("request.frame"), &raw.request)?;
            fs::write(raw.directory.join("response.frame"), &raw.response)?;
            fs::write(raw.directory.join("body.bin"), &raw.body)?;
        }
        counts.raw.clear();
        Ok(())
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
struct Active<'a>(&'a Shared);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        let mut c = self.0.0.lock().unwrap();
        c.active -= 1;
        self.0.1.notify_all();
    }
}
#[cfg(test)]
fn frame(stream: &mut impl Read) -> Result<(Vec<u8>, Value)> {
    let mut prefix = [0u8; 4];
    stream.read_exact(&mut prefix)?;
    let size = u32::from_be_bytes(prefix) as usize;
    ensure!(size > 0 && size <= MAX, "invalid JSON frame length");
    let mut bytes = vec![0u8; size + 4];
    bytes[..4].copy_from_slice(&prefix);
    stream.read_exact(&mut bytes[4..])?;
    let value = serde_json::from_slice(&bytes[4..])?;
    Ok((bytes, value))
}
fn retained_frame(stream: &mut impl Read, first: Option<u8>, bytes: &mut Vec<u8>) -> Result<Value> {
    bytes.extend(first);
    (&mut *stream)
        .take((4 - bytes.len()) as u64)
        .read_to_end(bytes)?;
    ensure!(bytes.len() == 4, "partial cache frame prefix");
    let size = u32::from_be_bytes(bytes[..4].try_into()?) as usize;
    ensure!(size > 0 && size <= MAX, "invalid cache JSON frame length");
    (&mut *stream).take(size as u64).read_to_end(bytes)?;
    ensure!(bytes.len() == size + 4, "partial cache frame body");
    let value = serde_json::from_slice(&bytes[4..])?;
    Ok(value)
}
fn exchange(
    mut client: Box<dyn Stream>,
    upstream: &str,
    evidence: &Path,
    id: u64,
    counts: &Shared,
    stop: &AtomicBool,
) -> Result<()> {
    let mut server: Option<TcpStream> = None;
    let mut version = None;
    let mut sequence = 0;
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut first = [0u8; 1];
        match client.read(&mut first) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
        counts.0.lock().unwrap().active += 1;
        let _active = Active(counts);
        let started = process::now_ns();
        sequence += 1;
        let dir = evidence.join(format!("{id:06}-{sequence:06}"));
        let mut capture = Capture {
            counts: counts.clone(),
            raw: Some(Raw {
                directory: dir.clone(),
                ..Raw::default()
            }),
        };
        let raw = capture.raw.as_mut().unwrap();
        let envelope = retained_frame(&mut client, Some(first[0]), &mut raw.request)?;
        let current = envelope["version"]
            .as_u64()
            .context("missing protocol version")?;
        ensure!(
            (current == 1 || current == 2) && version.is_none_or(|v| v == current),
            "invalid/changing cache version"
        );
        version = Some(current);
        let request = &envelope["request"];
        let op = request["op"].as_str().context("missing op")?;
        ensure!(
            [
                "ping", "open", "prepare", "list", "stat", "metadata", "read"
            ]
            .contains(&op),
            "unsupported cache op"
        );
        if server.is_none() {
            let s = TcpStream::connect_timeout(&upstream.parse()?, Duration::from_secs(90))?;
            s.set_nodelay(true)?;
            s.set_read_timeout(Some(Duration::from_secs(90)))?;
            s.set_write_timeout(Some(Duration::from_secs(90)))?;
            server = Some(s);
        }
        let server = server.as_mut().unwrap();
        server.write_all(&raw.request)?;
        let response = retained_frame(server, None, &mut raw.response)?;
        let status = response["status"]
            .as_str()
            .context("missing response status")?;
        let expected = match op {
            "ping" => "ready",
            "open" | "prepare" => "prepared",
            "list" => "entries",
            "stat" => "metadata",
            _ => "data",
        };
        ensure!(
            status == expected,
            "cache server returned failure or wrong operation response"
        );
        if status == "prepared" {
            ensure!(
                response["digest"] == source::SOURCE && response["architecture"] == "amd64",
                "cache prepared wrong image/platform"
            );
            if op == "open" {
                ensure!(
                    response["image_handle"] == request["handle"],
                    "cache open returned different handle"
                );
            }
            if op == "prepare" {
                ensure!(
                    request["image"]
                        .as_str()
                        .is_some_and(|s| s.ends_with(source::SOURCE)),
                    "cache prepare source differs"
                );
            }
        }
        let length = if status == "data" {
            response["length"].as_u64().context("invalid data length")? as usize
        } else {
            0
        };
        ensure!(length <= MAX, "oversized data body");
        if status == "data" {
            ensure!(
                length as u64
                    <= request["length"]
                        .as_u64()
                        .context("missing requested data length")?,
                "response exceeds requested length"
            );
        }
        (&mut *server)
            .take(length as u64)
            .read_to_end(&mut raw.body)?;
        ensure!(raw.body.len() == length, "partial cache Data body");
        let hash = format!("sha256:{}", source::bytes_digest(&raw.body));
        if status == "data" {
            ensure!(response["sha256"] == hash, "server payload digest mismatch");
        }
        // Raw request contains authentication; private evidence is mode 0700.
        // Never serialize a replacement response or forward a partial body.
        client.write_all(&raw.response)?;
        client.write_all(&raw.body)?;
        let finished = process::now_ns();
        let row = json!({"connection_id":id,"version":current,"op":op,"path":request.get("path"),"request":request,
            "response":response,"content_bytes":if op=="read"{length}else{0},"metadata_bytes":if op=="metadata"{length}else{0},
            "response_bytes":raw.response.len()+length,"forwarded_body_sha256":hash,"server_body_sha256":response.get("sha256"),
            "response_finished_ns":finished,"elapsed_ns":finished-started,"raw_dir":dir,
            "request_sha256":source::bytes_digest(&raw.request),"response_sha256":source::bytes_digest(&raw.response)});
        counts.0.lock().unwrap().rows.push(row);
        if current == 1 {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wire(value: &Value) -> Vec<u8> {
        let body = serde_json::to_vec(value).unwrap();
        let mut wire = (body.len() as u32).to_be_bytes().to_vec();
        wire.extend(body);
        wire
    }
    #[test]
    fn frame_rejects_zero_oversize_and_partial() {
        for body in [
            vec![0, 0, 0, 0],
            ((MAX + 1) as u32).to_be_bytes().to_vec(),
            vec![0, 0, 0, 5, b'{'],
        ] {
            assert!(frame(&mut body.as_slice()).is_err());
        }
    }
    #[test]
    fn forwards_exact_frames_and_bodies_and_counts_metadata() {
        let root = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let payload = b"portable metadata";
        let response = json!({"status":"data","length":payload.len(),"sha256":format!("sha256:{}",source::bytes_digest(payload))});
        let expected = wire(&response);
        let server_expected = expected.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (_, req) = frame(&mut stream).unwrap();
            assert_eq!(req["request"]["op"], "metadata");
            // Fragment both frame and body to test exact accumulation.
            for b in server_expected {
                stream.write_all(&[b]).unwrap();
            }
            for b in payload {
                stream.write_all(&[*b]).unwrap();
            }
        });
        let mut proxy =
            Proxy::start_tcp(&address.to_string(), &root.path().join("frames")).unwrap();
        let mut client = TcpStream::connect(proxy.address().trim_start_matches("tcp://")).unwrap();
        client.write_all(&wire(&json!({"version":2,"token":"private","request":{"op":"metadata","length":payload.len(),"handle":"immutable"}}))).unwrap();
        let (raw, got) = frame(&mut client).unwrap();
        assert_eq!(raw, expected);
        assert_eq!(got, response);
        let mut body = vec![0; payload.len()];
        client.read_exact(&mut body).unwrap();
        assert_eq!(body, payload);
        drop(client);
        server.join().unwrap();
        proxy.stop().unwrap();
        let traffic = proxy.observation(0, root.path()).unwrap();
        assert_eq!(traffic.metadata_bytes, payload.len() as u64);
        assert_eq!(traffic.content_bytes, 0);
        assert_eq!(
            traffic.response_bytes,
            (expected.len() + payload.len()) as u64
        );
        assert_eq!(
            fs::read(root.path().join("frames/000001-000001/body.bin")).unwrap(),
            payload
        );
    }
    #[test]
    fn corrupt_server_body_is_not_forwarded_or_valid() {
        let root = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            frame(&mut stream).unwrap();
            stream.write_all(&wire(&json!({"status":"data","length":3,"sha256":format!("sha256:{}","0".repeat(64))}))).unwrap();
            stream.write_all(b"bad").unwrap();
        });
        let mut proxy =
            Proxy::start_tcp(&address.to_string(), &root.path().join("frames")).unwrap();
        let mut client = TcpStream::connect(proxy.address().trim_start_matches("tcp://")).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client
            .write_all(&wire(
                &json!({"version":2,"request":{"op":"read","length":3}}),
            ))
            .unwrap();
        let mut data = Vec::new();
        client.read_to_end(&mut data).unwrap();
        assert!(data.is_empty());
        drop(client);
        server.join().unwrap();
        assert!(proxy.stop().is_err());
        assert!(proxy.observation(0, root.path()).is_err());
        assert_eq!(
            fs::read(root.path().join("frames/000001-000001/body.bin")).unwrap(),
            b"bad"
        );
    }
}
