//! Owned local S3 fixture with independent SigV4 verification and CAS semantics.
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let key = if key.len() > 64 {
        Sha256::digest(key).to_vec()
    } else {
        key.to_vec()
    };
    let mut inner = [0x36; 64];
    let mut outer = [0x5c; 64];
    for (i, byte) in key.iter().enumerate() {
        inner[i] ^= byte;
        outer[i] ^= byte;
    }
    let mut hash = Sha256::new();
    hash.update(inner);
    hash.update(data);
    let hashed = hash.finalize();
    let mut hash = Sha256::new();
    hash.update(outer);
    hash.update(hashed);
    hash.finalize().to_vec()
}
fn verify_signature(method: &str, path: &str, headers: &BTreeMap<String, String>, body: &[u8]) {
    assert_eq!(
        headers.get("x-amz-security-token").map(String::as_str),
        Some("test-token")
    );
    let authorization = headers
        .get("authorization")
        .expect("S3 request must be signed");
    let parts: BTreeMap<_, _> = authorization
        .strip_prefix("AWS4-HMAC-SHA256 ")
        .unwrap()
        .split(", ")
        .map(|part| part.split_once('=').unwrap())
        .collect();
    let (credential, scope) = parts["Credential"].split_once('/').unwrap();
    assert_eq!(credential, "AKIATEST");
    let scope_parts: Vec<_> = scope.split('/').collect();
    assert_eq!(&scope_parts[1..], &["us-east-1", "s3", "aws4_request"]);
    let signed = parts["SignedHeaders"];
    let canonical_headers: String = signed
        .split(';')
        .map(|name| {
            format!(
                "{name}:{}\n",
                headers[name]
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })
        .collect();
    let payload = &headers["x-amz-content-sha256"];
    if payload != "UNSIGNED-PAYLOAD" {
        assert_eq!(payload, &digest(body));
    }
    let (uri, query) = path.split_once('?').unwrap_or((path, ""));
    let canonical = format!("{method}\n{uri}\n{query}\n{canonical_headers}\n{signed}\n{payload}");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
        headers["x-amz-date"],
        digest(canonical.as_bytes())
    );
    let date = hmac(b"AWS4test-secret", scope_parts[0].as_bytes());
    let region = hmac(&date, b"us-east-1");
    let service = hmac(&region, b"s3");
    let key = hmac(&service, b"aws4_request");
    assert_eq!(parts["Signature"], hex(&hmac(&key, to_sign.as_bytes())));
}
pub(crate) struct MockS3 {
    pub(crate) endpoint: String,
    pub(crate) objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    pub(crate) read_only: Arc<AtomicBool>,
    pub(crate) deny_reads: Arc<AtomicBool>,
    pub(crate) lost_head_ack: Arc<AtomicBool>,
    #[allow(dead_code)] // Used by the opt-in native Worker crash gate.
    pub(crate) pause_checkpoint_put: Arc<AtomicBool>,
    #[allow(dead_code)] // Used by the opt-in native Worker crash gate.
    pub(crate) checkpoint_put_waiting: Arc<AtomicBool>,
    pub(crate) puts: Arc<AtomicUsize>,
    pub(crate) gets: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
struct Faults {
    lost_head_ack: Arc<AtomicBool>,
    pause_checkpoint_put: Arc<AtomicBool>,
    checkpoint_put_waiting: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}
impl MockS3 {
    pub(crate) fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let objects = Arc::new(Mutex::new(BTreeMap::<String, Vec<u8>>::new()));
        let read_only = Arc::new(AtomicBool::new(false));
        let deny_reads = Arc::new(AtomicBool::new(false));
        let lost_head_ack = Arc::new(AtomicBool::new(false));
        let puts = Arc::new(AtomicUsize::new(0));
        let gets = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_objects = objects.clone();
        let worker_read_only = read_only.clone();
        let worker_deny_reads = deny_reads.clone();
        let pause_checkpoint_put = Arc::new(AtomicBool::new(false));
        let checkpoint_put_waiting = Arc::new(AtomicBool::new(false));
        let faults = Faults {
            lost_head_ack: lost_head_ack.clone(),
            pause_checkpoint_put: pause_checkpoint_put.clone(),
            checkpoint_put_waiting: checkpoint_put_waiting.clone(),
            stop: stop.clone(),
        };
        let worker_puts = puts.clone();
        let worker_gets = gets.clone();
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // BSD/macOS can inherit the listener's nonblocking mode.
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        serve_s3(
                            &mut stream,
                            &worker_objects,
                            &worker_read_only,
                            &worker_deny_reads,
                            &worker_puts,
                            &worker_gets,
                            &faults,
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("S3 fixture: {error}"),
                }
            }
        });
        Self {
            endpoint,
            objects,
            read_only,
            deny_reads,
            lost_head_ack,
            pause_checkpoint_put,
            checkpoint_put_waiting,
            puts,
            gets,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for MockS3 {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn serve_s3(
    stream: &mut TcpStream,
    objects: &Mutex<BTreeMap<String, Vec<u8>>>,
    read_only: &AtomicBool,
    deny_reads: &AtomicBool,
    puts: &AtomicUsize,
    gets: &AtomicUsize,
    faults: &Faults,
) {
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0; 8192];
        let size = stream.read(&mut chunk).unwrap();
        assert!(size > 0);
        bytes.extend_from_slice(&chunk[..size]);
        if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
            break end + 4;
        }
        assert!(bytes.len() < 1024 * 1024);
    };
    let text = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let mut lines = text.lines();
    let first: Vec<_> = lines.next().unwrap().split_whitespace().collect();
    let headers: BTreeMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().into()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .map(|v| v.parse().unwrap())
        .unwrap_or(0);
    while bytes.len() < header_end + length {
        let mut chunk = [0; 16384];
        let size = stream.read(&mut chunk).unwrap();
        assert!(size > 0);
        bytes.extend_from_slice(&chunk[..size]);
    }
    let body = &bytes[header_end..header_end + length];
    verify_signature(first[0], first[1], &headers, body);
    assert!(first[1].starts_with("/cache-bucket/team/"));
    let mut objects = objects.lock().unwrap();
    let mut content_range = String::new();
    let etag = objects
        .get(first[1])
        .map(|body| digest(body))
        .unwrap_or_else(|| digest(body));
    let (status, response) = match first[0] {
        "PUT" => {
            puts.fetch_add(1, Ordering::Relaxed);
            if read_only.load(Ordering::Relaxed) {
                (
                    "403 Forbidden",
                    b"<Error><Code>AccessDenied</Code></Error>".to_vec(),
                )
            } else if (headers.get("if-none-match").map(String::as_str) == Some("*")
                && objects.contains_key(first[1]))
                || headers.get("if-match").is_some_and(|expected| {
                    objects
                        .get(first[1])
                        .is_none_or(|body| *expected != format!("\"{}\"", digest(body)))
                })
            {
                (
                    "412 Precondition Failed",
                    b"<Error><Code>PreconditionFailed</Code></Error>".to_vec(),
                )
            } else {
                objects.insert(first[1].into(), body.to_vec());
                if first[1].ends_with("/HEAD.json")
                    && faults.lost_head_ack.swap(false, Ordering::Relaxed)
                {
                    // Commit succeeded but the acknowledgement never arrived.
                    return;
                }
                ("200 OK", Vec::new())
            }
        }
        "GET" => {
            gets.fetch_add(1, Ordering::Relaxed);
            if deny_reads.load(Ordering::Relaxed) {
                (
                    "403 Forbidden",
                    b"<Error><Code>AccessDenied</Code></Error>".to_vec(),
                )
            } else {
                match objects.get(first[1]) {
                    Some(body) => {
                        if read_only.load(Ordering::Relaxed)
                            && first[1].ends_with(".bin")
                            && !first[1].ends_with("checksums.bin")
                        {
                            assert!(
                                headers.contains_key("range"),
                                "binary metadata must use ranged reads"
                            );
                        }
                        if let Some(range) = headers.get("range") {
                            let (start, end) = range
                                .strip_prefix("bytes=")
                                .unwrap()
                                .split_once('-')
                                .unwrap();
                            let start: usize = start.parse().unwrap();
                            let end: usize = end.parse().unwrap();
                            assert!(start <= end && end < body.len());
                            content_range =
                                format!("Content-Range: bytes {start}-{end}/{}\r\n", body.len());
                            ("206 Partial Content", body[start..=end].to_vec())
                        } else {
                            ("200 OK", body.clone())
                        }
                    }
                    None => (
                        "404 Not Found",
                        b"<Error><Code>NoSuchKey</Code></Error>".to_vec(),
                    ),
                }
            }
        }
        method => {
            panic!("unexpected S3 operation {method}; cache should only need GetObject/PutObject")
        }
    };
    drop(objects);
    if first[0] == "PUT"
        && status == "200 OK"
        && first[1].contains("/pvisor-checkpoints-v1/chunks/")
        && faults.pause_checkpoint_put.load(Ordering::SeqCst)
    {
        // Independently verified and durably committed, but no acknowledgement
        // until the test kills the source Worker. A retry must compare/reuse it.
        faults.checkpoint_put_waiting.store(true, Ordering::SeqCst);
        while faults.pause_checkpoint_put.load(Ordering::SeqCst)
            && !faults.stop.load(Ordering::Relaxed)
        {
            std::thread::sleep(Duration::from_millis(1));
        }
        return;
    }
    write!(stream, "HTTP/1.1 {status}\r\n{content_range}Content-Length: {}\r\nETag: \"{}\"\r\nLast-Modified: Wed, 01 Jan 2025 00:00:00 GMT\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n", response.len(), etag).unwrap();
    stream.write_all(&response).unwrap();
}
