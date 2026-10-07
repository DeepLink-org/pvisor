//! Real sockets exercise global admission and concurrent progress while remote
//! GETs are stalled. Authentication is covered by the independent SigV4 fixture.
use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
struct Remote {
    url: String,
    arrived: mpsc::Receiver<()>,
    release: Arc<(Mutex<bool>, Condvar)>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Remote {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let (entered, arrived) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = std::thread::spawn({
            let release = release.clone();
            let stop = stop.clone();
            move || {
                let mut handlers = vec![];
                while !stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            // BSD/macOS can inherit the listener's nonblocking
                            // mode; handlers require blocking, timed reads.
                            stream.set_nonblocking(false).unwrap();
                            let entered = entered.clone();
                            let release = release.clone();
                            handlers.push(std::thread::spawn(move || {
                                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                                let mut headers = Vec::new();
                                while !headers.windows(4).any(|s| s == b"\r\n\r\n") {
                                    let mut buffer = [0;1024]; let read = stream.read(&mut buffer).unwrap();
                                    assert!(read > 0); headers.extend_from_slice(&buffer[..read]);
                                    assert!(headers.len() < 16*1024);
                                }
                                assert!(headers.starts_with(b"GET "));
                                let _ = entered.send(());
                                let mut open = release.0.lock().unwrap();
                                while !*open { open = release.1.wait(open).unwrap(); }
                                drop(open);
                                let body = b"network-value";
                                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"fixed\"\r\nLast-Modified: Wed, 01 Jan 2025 00:00:00 GMT\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                                stream.write_all(body).unwrap();
                            }));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1))
                        }
                        Err(error) => panic!("remote fixture: {error}"),
                    }
                }
                for handler in handlers {
                    handler.join().unwrap();
                }
            }
        });
        Self {
            url,
            arrived,
            release,
            stop,
            worker: Some(worker),
        }
    }
    fn open(&self) {
        *self.release.0.lock().unwrap() = true;
        self.release.1.notify_all();
    }
}
impl Drop for Remote {
    fn drop(&mut self) {
        self.open();
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}
#[test]
fn stalled_network_reads_run_concurrently_but_never_exceed_global_admission() {
    let remote = Remote::start();
    let runtime = Runtime::shared().unwrap();
    let store: Arc<dyn ObjectStore> = Arc::new(
        object_store::aws::AmazonS3Builder::new()
            .with_bucket_name("bounded")
            .with_region("us-east-1")
            .with_access_key_id("local-test-key")
            .with_secret_access_key("local-test-secret")
            .with_endpoint(&remote.url)
            .with_allow_http(true)
            .build()
            .unwrap(),
    );
    let ready = Arc::new(std::sync::Barrier::new(64));
    let callers: Vec<_> = (0..64)
        .map(|i| {
            let runtime = runtime.clone();
            let store = store.clone();
            let ready = ready.clone();
            std::thread::spawn(move || {
                ready.wait();
                let object = runtime
                    .request(store, format!("value-{i}"), Operation::Get(None))
                    .unwrap()
                    .unwrap();
                assert_eq!(object.bytes, b"network-value");
            })
        })
        .collect();
    // Concurrent socket progress is observable before any request can finish.
    for _ in 0..32 {
        remote.arrived.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    assert!(
        matches!(
            remote.arrived.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "more than 32 requests reached the remote while all responses were stalled"
    );
    remote.open();
    for caller in callers {
        caller.join().unwrap();
    }
    for _ in 0..32 {
        remote.arrived.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}
