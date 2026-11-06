//! Bounded read-only HTTP repositories, including local IPFS gateways.
use super::storage::StoredObject;
use anyhow::{Context, ensure};
use object_store::UpdateVersion;
use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

pub(crate) struct Http {
    root: reqwest::Url,
    send: Option<tokio::sync::mpsc::UnboundedSender<Task>>,
    worker: Option<std::thread::JoinHandle<()>>,
    active: Arc<AtomicUsize>,
    pid: u32,
}

struct Task {
    url: reqwest::Url,
    range: Option<Range<u64>>,
    limit: usize,
    reply: std::sync::mpsc::SyncSender<anyhow::Result<Option<StoredObject>>>,
    permit: Permit,
    deadline: std::time::Instant,
}

struct Permit(Arc<AtomicUsize>);

#[derive(Debug, thiserror::Error)]
#[error("HTTP image source temporarily unavailable")]
struct RetryLater(Duration);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Drop for Http {
    fn drop(&mut self) {
        self.send.take();
        if self.pid == std::process::id()
            && let Some(worker) = self.worker.take()
        {
            let _ = worker.join();
        }
    }
}

impl Http {
    pub(super) fn validate(address: &str) -> anyhow::Result<reqwest::Url> {
        if !address.starts_with("ipfs+") {
            let mut root = reqwest::Url::parse(address)?;
            ensure!(
                matches!(root.scheme(), "http" | "https")
                    && root.host_str().is_some()
                    && root.username().is_empty()
                    && root.password().is_none()
                    && root.query().is_none()
                    && root.fragment().is_none()
                    && !root.path().contains('%')
                    && !address.contains('%')
                    && address.split('/').all(|part| part != "." && part != "..")
                    && !address.contains('\\'),
                "HTTP repository must be an absolute http(s) URL without credentials, query or fragment"
            );
            let path = format!("{}/", root.path().trim_end_matches('/'));
            root.set_path(&path);
            return Ok(root);
        }
        let url = address
            .strip_prefix("ipfs+")
            .context("expected ipfs+http://LOOPBACK:PORT/ipfs/CID")?;
        let mut root = reqwest::Url::parse(url)?;
        ensure!(
            root.scheme() == "http"
                && root.host_str().is_some_and(|host| {
                    host.parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
                })
                && root.username().is_empty()
                && root.password().is_none()
                && root.query().is_none()
                && root.fragment().is_none(),
            "IPFS requires a trusted literal-loopback HTTP gateway"
        );
        let path = root.path().trim_end_matches('/');
        let cid = path.strip_prefix("/ipfs/").context("expected /ipfs/CID")?;
        // Kubo validates the full CID. Exclude paths, IPNS and URL metacharacters.
        ensure!(
            (32..=128).contains(&cid.len()) && cid.bytes().all(|b| b.is_ascii_alphanumeric()),
            "invalid immutable IPFS directory CID"
        );
        root.set_path(&format!("{path}/"));
        Ok(root)
    }

    pub(super) fn new(address: &str) -> anyhow::Result<Self> {
        let root = Self::validate(address)?;
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30));
        if root.scheme() == "https"
            && let Some(path) = std::env::var_os("SSL_CERT_FILE")
        {
            let pem = std::fs::read(path).context("read image SSL_CERT_FILE")?;
            let certificates = reqwest::Certificate::from_pem_bundle(&pem)?;
            ensure!(
                !certificates.is_empty(),
                "image SSL_CERT_FILE contains no certificates"
            );
            for certificate in certificates {
                builder = builder.add_root_certificate(certificate);
            }
        }
        let client = builder.build()?;
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel::<Task>();
        let (ready, initialized) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new().name("pvisor-image-http".into()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => { let _ = ready.send(Err(error)); return; }
            };
            let _ = ready.send(Ok(()));
            runtime.block_on(async move {
                let mut tasks = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        task = receive.recv() => match task {
                            Some(task) => {
                                let client = client.clone();
                                tasks.spawn(async move {
                                    let result = Self::fetch_with_deadline(&client, task.url, task.range, task.limit,
                                        task.deadline.saturating_duration_since(std::time::Instant::now())).await;
                                    let _ = task.reply.send(result);
                                    drop(task.permit);
                                });
                            }
                            None => break,
                        },
                        _ = tasks.join_next(), if !tasks.is_empty() => {}
                    }
                }
                while tasks.join_next().await.is_some() {}
            });
        })?;
        initialized
            .recv()
            .context("HTTP image I/O worker initialization failed")??;
        Ok(Self {
            root,
            send: Some(send),
            worker: Some(worker),
            active: Arc::new(AtomicUsize::new(0)),
            pid: std::process::id(),
        })
    }

    pub(super) fn read(
        &self,
        key: &str,
        range: Option<Range<u64>>,
        limit: usize,
    ) -> anyhow::Result<Option<StoredObject>> {
        self.read_timed(key, range, limit, Duration::from_secs(30))
    }

    pub(super) fn read_timed(
        &self,
        key: &str,
        range: Option<Range<u64>>,
        limit: usize,
        budget: Duration,
    ) -> anyhow::Result<Option<StoredObject>> {
        ensure!(
            self.pid == std::process::id(),
            "reopen HTTP image client after fork"
        );
        ensure!(
            self.active
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < 32)
                    .then_some(n + 1))
                .is_ok(),
            "HTTP image read queue saturated"
        );
        let permit = Permit(self.active.clone());
        super::storage::validate_key(key)?;
        let mut url = self.root.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| anyhow::anyhow!("invalid gateway"))?;
            segments.pop_if_empty();
            for segment in key.split('/') {
                segments.push(segment);
            }
        }
        let (reply, receive) = std::sync::mpsc::sync_channel(1);
        self.send
            .as_ref()
            .context("HTTP image worker stopped")?
            .send(Task {
                url,
                range,
                limit,
                reply,
                permit,
                deadline: std::time::Instant::now() + budget,
            })
            .map_err(|_| anyhow::anyhow!("HTTP image worker stopped"))?;
        receive
            .recv()
            .context("HTTP image worker dropped response")?
    }

    async fn fetch_with_deadline(
        client: &reqwest::Client,
        url: reqwest::Url,
        range: Option<Range<u64>>,
        limit: usize,
        budget: Duration,
    ) -> anyhow::Result<Option<StoredObject>> {
        let deadline = tokio::time::Instant::now() + budget;
        for execution in 0..2 {
            let result = tokio::time::timeout_at(
                deadline,
                Self::fetch_once(client, url.clone(), range.clone(), limit),
            )
            .await
            .map_err(|_| anyhow::anyhow!("HTTP image read timed out"))?;
            match result {
                Err(error) if execution == 0 => {
                    let wait = if let Some(retry) = error.downcast_ref::<RetryLater>() {
                        Some(retry.0)
                    } else if error.downcast_ref::<reqwest::Error>().is_some_and(|e| {
                        e.is_connect()
                            || e.is_timeout()
                            || e.is_body()
                            || e.is_request()
                            || e.is_decode()
                    }) {
                        Some(Duration::from_millis(100))
                    } else {
                        None
                    };
                    if let Some(wait) = wait {
                        tokio::time::timeout_at(deadline, tokio::time::sleep(wait))
                            .await
                            .map_err(|_| anyhow::anyhow!("HTTP image retry budget exhausted"))?;
                    } else {
                        return Err(error);
                    }
                }
                result => return result,
            }
        }
        unreachable!()
    }

    async fn fetch_once(
        client: &reqwest::Client,
        url: reqwest::Url,
        range: Option<Range<u64>>,
        limit: usize,
    ) -> anyhow::Result<Option<StoredObject>> {
        if let Some(range) = &range {
            ensure!(
                range.end > range.start && range.end - range.start <= limit as u64,
                "invalid HTTP image range"
            );
        }
        let mut request = client.get(url).header("Accept-Encoding", "identity");
        if let Some(range) = &range {
            request = request.header("Range", format!("bytes={}-{}", range.start, range.end - 1));
        }
        let response = request.send().await.context("read HTTP image object")?;
        if matches!(response.status().as_u16(), 429 | 500 | 502 | 503 | 504) {
            let wait = match response.headers().get("Retry-After") {
                None => Duration::from_millis(100),
                Some(value) => {
                    let seconds = value
                        .to_str()?
                        .parse::<u64>()
                        .context("unsupported Retry-After; request not retried early")?;
                    ensure!(seconds <= 1, "Retry-After exceeds image retry budget");
                    Duration::from_secs(seconds)
                }
            };
            return Err(RetryLater(wait).into());
        }
        // A missing dependency is corruption/unavailability, not a missing file.
        // Do not convert gateway errors (including 404) into filesystem ENOENT.
        let mut response = response.error_for_status()?;
        ensure!(
            response
                .headers()
                .get("Content-Encoding")
                .is_none_or(|value| value == "identity"),
            "HTTP image transformations are forbidden"
        );
        let expected = if let Some(range) = range {
            ensure!(
                response.status() == reqwest::StatusCode::PARTIAL_CONTENT,
                "HTTP image server ignored Range"
            );
            let value = response
                .headers()
                .get("Content-Range")
                .context("missing Content-Range")?
                .to_str()?;
            let prefix = format!("bytes {}-{}/", range.start, range.end - 1);
            let total: u64 = value
                .strip_prefix(&prefix)
                .context("incorrect Content-Range")?
                .parse()?;
            ensure!(total >= range.end, "invalid HTTP image object length");
            Some(usize::try_from(range.end - range.start)?)
        } else {
            ensure!(
                response.status() == reqwest::StatusCode::OK,
                "unexpected HTTP image response"
            );
            None
        };
        let bound = expected.unwrap_or(limit).min(limit);
        ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= bound as u64),
            "HTTP image object exceeds read limit"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                chunk.len() <= bound.saturating_sub(bytes.len()),
                "HTTP image object exceeds read limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        if let Some(expected) = expected {
            ensure!(bytes.len() == expected, "truncated HTTP image range");
        }
        Ok(Some(StoredObject {
            bytes,
            version: UpdateVersion {
                e_tag: None,
                version: None,
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_repository_validation_and_read_only_policy() {
        for address in [
            "https://mirror.example/pvisor",
            "http://127.0.0.1:8080/root/",
        ] {
            assert!(Http::validate(address).unwrap().path().ends_with('/'));
        }
        for address in [
            "file:///tmp/pool",
            "https://user:secret@example/root",
            "https://example/root?token=secret",
            "https://example/root#fragment",
            "https://example/a%2fb",
            "https://example/a/../b",
            "https://example/a/%2e%2e/b",
            "http:///",
        ] {
            assert!(Http::validate(address).is_err(), "{address}");
        }
        let storage = super::super::storage::Storage::Http(std::sync::Arc::new(
            Http::new("http://127.0.0.1:9").unwrap(),
        ));
        assert!(
            storage
                .put("image.json", b"private".to_vec(), true)
                .is_err()
        );
    }

    #[test]
    fn queue_saturation_and_invalid_keys_never_reach_network() {
        let client = Http::new("http://127.0.0.1:9").unwrap();
        client.active.store(32, Ordering::Release);
        assert!(
            client
                .read("format.json", None, 1024)
                .err()
                .unwrap()
                .to_string()
                .contains("saturated")
        );
        client.active.store(0, Ordering::Release);
        assert!(client.read("../escape", None, 1024).is_err());
        assert_eq!(client.active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn confines_gateway_and_requires_immutable_root() {
        let cid = "b".repeat(59);
        assert!(Http::new(&format!("ipfs+http://127.0.0.1:8080/ipfs/{cid}")).is_ok());
        for root in [
            format!("ipfs+http://example.com/ipfs/{cid}"),
            format!("ipfs+http://127.0.0.1/ipns/{cid}"),
            format!("ipfs+http://127.0.0.1/ipfs/{cid}/nested"),
            format!("ipfs+http://user@127.0.0.1/ipfs/{cid}"),
            format!("ipfs+http://127.0.0.1/ipfs/{cid}?x=1"),
        ] {
            assert!(Http::new(&root).is_err(), "{root}");
        }
    }

    fn response(
        status: &str,
        headers: &str,
        body: &[u8],
    ) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{BufRead, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!(
            "ipfs+http://{}/ipfs/{}",
            listener.local_addr().unwrap(),
            "b".repeat(59)
        );
        let status = status.to_owned();
        let headers = headers.to_owned();
        let body = body.to_vec();
        let task = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                request.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
            request
        });
        (address, task)
    }

    #[test]
    fn exact_range_is_required_and_body_is_bounded() {
        let (address, task) = response(
            "206 Partial Content",
            "Content-Range: bytes 2-4/10\r\n",
            b"abc",
        );
        let bytes = Http::new(&address)
            .unwrap()
            .read("data/file", Some(2..5), 64)
            .unwrap()
            .unwrap()
            .bytes;
        assert_eq!(bytes, b"abc");
        let request = task.join().unwrap().to_lowercase();
        assert!(request.contains("range: bytes=2-4"));
        assert!(request.contains("/data/file "));
        for (status, headers, body, range, limit) in [
            ("200 OK", "", b"abc".as_slice(), Some(2..5), 64),
            (
                "206 Partial Content",
                "Content-Range: bytes 1-3/10\r\n",
                b"abc".as_slice(),
                Some(2..5),
                64,
            ),
            (
                "206 Partial Content",
                "Content-Range: bytes 2-4/10\r\n",
                b"ab".as_slice(),
                Some(2..5),
                64,
            ),
            ("200 OK", "", b"abcd".as_slice(), None, 3),
            ("404 Not Found", "", b"missing".as_slice(), None, 64),
            ("401 Unauthorized", "", b"denied".as_slice(), None, 64),
            (
                "503 Unavailable",
                "Retry-After: 60\r\n",
                b"".as_slice(),
                None,
                64,
            ),
            (
                "200 OK",
                "Content-Encoding: gzip\r\n",
                b"abc".as_slice(),
                None,
                64,
            ),
            (
                "302 Found",
                "Location: http://example.com/\r\n",
                b"".as_slice(),
                None,
                64,
            ),
        ] {
            let (address, task) = response(status, headers, body);
            assert!(
                Http::new(&address)
                    .unwrap()
                    .read("file", range, limit)
                    .is_err()
            );
            task.join().unwrap();
        }
    }

    #[tokio::test]
    async fn client_can_be_created_used_and_dropped_inside_tokio() {
        let (address, task) = response("200 OK", "", b"ok");
        let client = Http::new(&address).unwrap();
        assert_eq!(client.read("file", None, 8).unwrap().unwrap().bytes, b"ok");
        drop(client);
        task.join().unwrap();
    }

    #[test]
    fn per_source_deadline_reaches_the_worker_and_later_reads_recover() {
        use std::io::{BufRead, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let location = format!("http://{}", listener.local_addr().unwrap());
        let task = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(200));
            drop(stream);
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });
        let client = Http::new(&location).unwrap();
        assert!(
            client
                .read_timed("stalled", None, 16, Duration::from_millis(50))
                .is_err()
        );
        assert_eq!(
            client
                .read_timed("recovered", None, 16, Duration::from_secs(2))
                .unwrap()
                .unwrap()
                .bytes,
            b"ok"
        );
        let active = client.active.clone();
        drop(client);
        assert_eq!(active.load(Ordering::Acquire), 0);
        task.join().unwrap();
    }

    // Low-pressure deterministic fault injection: one socket, <=2 requests per
    // case, tiny bodies. No public services, stress loops or global env edits.
    #[tokio::test]
    async fn chaos_http_retries_once_and_bounds_stalls() {
        use std::io::{BufRead, Write};
        for first in [
            b"".as_slice(),
            b"HTTP/1.1 503 Unavailable\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nbad",
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = reqwest::Url::parse(&format!("http://{}/value", listener.local_addr().unwrap())).unwrap();
            let first = first.to_vec();
            let task = std::thread::spawn(move || {
                for body in [first, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec()] {
                    let (mut stream, _) = listener.accept().unwrap();
                    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" || line.is_empty() { break; }
                    }
                    let _ = stream.write_all(&body);
                }
            });
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let bytes = Http::fetch_with_deadline(&client, url, None, 16, Duration::from_secs(2))
                .await.unwrap().unwrap().bytes;
            assert_eq!(bytes, b"ok");
            task.join().unwrap();
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = reqwest::Url::parse(&format!(
            "http://{}/stalled",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let task = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(200));
        });
        let start = std::time::Instant::now();
        assert!(
            Http::fetch_with_deadline(
                &reqwest::Client::new(),
                url,
                None,
                16,
                Duration::from_millis(50)
            )
            .await
            .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        task.join().unwrap();
    }
}
