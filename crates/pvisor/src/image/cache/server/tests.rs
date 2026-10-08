//! Service confinement, protocol, and client/server regression tests.
use super::*;
use crate::image::cache::CacheClient;
use crate::image::cache::client::tests::{client_at_with_reuse, use_server_reuse};
use crate::image::cache::protocol::hash;
use std::os::unix::fs::symlink;

fn fixture() -> (tempfile::TempDir, ImageStore, String) {
    let tmp = tempfile::tempdir().unwrap();
    let store = ImageStore::new(Some(tmp.path().join("store"))).unwrap();
    let digest = format!("sha256:{}", "a".repeat(64));
    let root = store.root.join("rootfs-v3/sha256").join("a".repeat(64));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("hello"), b"hello world").unwrap();
    fs::create_dir(root.join("dir")).unwrap();
    symlink("hello", root.join("alias")).unwrap();
    symlink("/etc", root.join("escape")).unwrap();
    (tmp, store, digest)
}

#[test]
fn prepare_queue_is_bounded_and_does_not_block_ping() {
    let (_tmp, store, _) = fixture();
    let store = reader(&store).unwrap();
    let (queue, pending) = mpsc::sync_channel(1);
    let exchange = |request| {
        let (mut client, server) = UnixStream::pair().unwrap();
        write_frame(
            &mut client,
            &Envelope {
                version: 1,
                token: None,
                request,
            },
        )
        .unwrap();
        serve_connection(Stream::Unix(server), &store, None, Some(&queue)).unwrap();
        client
    };
    let prepare = || Request::Prepare {
        image: "ubuntu:latest".into(),
        architecture: "arm64".into(),
        refresh: false,
    };
    let _first = exchange(prepare());
    let response: Response = read_frame(&mut exchange(prepare())).unwrap();
    assert!(
        matches!(response, Response::Error { .. }),
        "full queue must reply rather than hang"
    );
    let response: Response = read_frame(&mut exchange(Request::Ping)).unwrap();
    assert!(matches!(response, Response::Ready));
    assert!(matches!(
        pending.try_recv().unwrap().1,
        Request::Prepare { .. }
    ));
}

#[test]
fn unix_client_roundtrip_and_parallel_reads() {
    let (tmp, store, digest) = fixture();
    let cache = reader(&store).unwrap();
    let image = crate::image::oci::PreparedImage {
        digest: digest.clone(),
        rootfs: store.root.join("rootfs-v3/sha256").join(&digest[7..]),
        env: Default::default(),
        entrypoint: vec![],
        cmd: vec![],
    };
    let (prepared, _) = cache.publish(&store, &image, "amd64", "fixture").unwrap();
    let Response::Prepared {
        image_handle: digest,
        ..
    } = prepared
    else {
        panic!()
    };
    let store = cache;
    let path = tmp.path().join("server.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = std::thread::spawn(move || {
        std::thread::scope(|scope| {
            for connection in listener.incoming().take(8) {
                let store = &store;
                scope.spawn(move || {
                    serve_connection(Stream::Unix(connection.unwrap()), store, None, None).unwrap()
                });
            }
        });
    });
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let address = format!("unix://{}", path.display());
            let digest = digest.clone();
            scope.spawn(move || {
                let client = use_server_reuse(CacheClient::new(address, None).unwrap(), true);
                let (_, body) = client
                    .request(Request::Read {
                        digest,
                        path: b"hello".to_vec(),
                        offset: 6,
                        length: 5,
                    })
                    .unwrap();
                assert_eq!(body, b"world");
            });
        }
    });
    server.join().unwrap();
}

#[test]
fn rejects_bad_frames_versions_and_tokens() {
    assert!(read_frame::<Envelope>(&mut &u32::MAX.to_be_bytes()[..]).is_err());
    assert!(endpoint("tcp://0.0.0.0:9000").is_err());
    assert!(CacheClient::new("tcp://127.0.0.1:9000".into(), None).is_err());
    for (version, token) in [
        (3, Some("secret")),
        (1, Some("wrong")),
        (1, None),
        (2, Some("wrong")),
        (2, None),
    ] {
        let (_tmp, store, digest) = fixture();
        let store = reader(&store).unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            serve_connection(Stream::Unix(server), &store, Some("secret"), None).unwrap()
        });
        write_frame(
            &mut client,
            &Envelope {
                version,
                token: token.map(str::to_owned),
                request: Request::Stat {
                    digest,
                    path: b"hello".to_vec(),
                },
            },
        )
        .unwrap();
        assert!(matches!(
            read_frame::<Response>(&mut client).unwrap(),
            Response::Error { .. }
        ));
        worker.join().unwrap();
    }
}

#[test]
fn client_rejects_corrupt_content() {
    for reuse in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("server.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            if reuse {
                for _ in 0..2 {
                    let ping: Envelope = read_frame(&mut socket).unwrap();
                    assert_eq!(ping.version, 2);
                    assert!(matches!(ping.request, Request::Ping));
                    write_frame(&mut socket, &Response::Ready).unwrap();
                }
            }
            let read: Envelope = read_frame(&mut socket).unwrap();
            assert_eq!(read.version, if reuse { 2 } else { 1 });
            assert!(matches!(read.request, Request::Read { length: 3, .. }));
            write_frame(
                &mut socket,
                &Response::Data {
                    length: 3,
                    sha256: hash(b"abc"),
                },
            )
            .unwrap();
            socket.write_all(b"bad").unwrap();
        });
        let client = client_at_with_reuse(&path, reuse);
        let error = client
            .request(Request::Read {
                digest: "unused".into(),
                path: b"file".to_vec(),
                offset: 0,
                length: 3,
            })
            .unwrap_err();
        assert!(
            error.to_string().contains("digest mismatch"),
            "reuse={reuse}: {error:#}"
        );
        worker.join().unwrap();
    }
}

#[test]
fn server_and_direct_readers_share_revisions_after_source_removal() {
    for reuse in [false, true] {
        let (tmp, store, digest) = fixture();
        let cache = reader(&store).unwrap();
        let image = crate::image::oci::PreparedImage {
            digest: digest.clone(),
            rootfs: store.root.join("rootfs-v3/sha256").join(&digest[7..]),
            env: Default::default(),
            entrypoint: vec![],
            cmd: vec![],
        };
        let (canonical, _) = crate::image::oci::cache_reference("fixture:test").unwrap();
        let (prepared, _) = cache.publish(&store, &image, "amd64", &canonical).unwrap();
        let Response::Prepared {
            image_handle: handle,
            ..
        } = prepared
        else {
            panic!()
        };
        fs::remove_dir_all(store.root.join("rootfs-v3")).unwrap();
        let direct = CacheClient::new(
            format!("file://{}", store.root.join("cache-v1").display()),
            None,
        )
        .unwrap();
        let socket = tmp.path().join("reader.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = vec![
            Request::Prepare {
                image: "fixture:test".into(),
                architecture: "amd64".into(),
                refresh: false,
            },
            Request::Open {
                handle: handle.clone(),
                architecture: "amd64".into(),
            },
            Request::List {
                digest: handle.clone(),
                path: vec![],
                offset: 0,
            },
            Request::Stat {
                digest: handle.clone(),
                path: b"alias".to_vec(),
            },
            Request::Read {
                digest: handle.clone(),
                path: b"hello".to_vec(),
                offset: 6,
                length: 5,
            },
        ];
        // V1 opens a connection for each actual request; V2 retains one through
        // negative/positive Stat, the semantic comparisons, and the final error.
        let count = if reuse { 1 } else { requests.len() + 3 };
        let server = std::thread::spawn(move || {
            for connection in listener.incoming().take(count) {
                serve_connection(Stream::Unix(connection.unwrap()), &cache, None, None).unwrap();
            }
        });
        let client = client_at_with_reuse(&socket, reuse);
        let error = client
            .request(Request::Stat {
                digest: handle.clone(),
                path: b"missing".to_vec(),
            })
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(matches!(
            client
                .request(Request::Stat {
                    digest: handle.clone(),
                    path: b"hello".to_vec(),
                })
                .unwrap()
                .0,
            Response::Metadata { size: 11, .. }
        ));
        for request in requests {
            let wire = serde_json::to_vec(&request).unwrap();
            let expected = direct
                .request(serde_json::from_slice(&wire).unwrap())
                .unwrap();
            let observed = client.request(request).unwrap();
            assert_eq!(
                serde_json::to_value(observed.0).unwrap(),
                serde_json::to_value(expected.0).unwrap()
            );
            assert_eq!(observed.1, expected.1);
        }
        assert!(
            client
                .request(Request::Open {
                    handle,
                    architecture: "arm64".into()
                })
                .is_err()
        );
        drop(client);
        server.join().unwrap();
    }
}

#[test]
fn tcp_persistent_file_bodies_and_ping_share_one_connection() {
    let (_tmp, store, digest) = fixture();
    let cache = reader(&store).unwrap();
    let image = crate::image::oci::PreparedImage {
        digest: digest.clone(),
        rootfs: store.root.join("rootfs-v3/sha256").join(&digest[7..]),
        env: Default::default(),
        entrypoint: vec![],
        cmd: vec![],
    };
    let (prepared, _) = cache.publish(&store, &image, "amd64", "fixture").unwrap();
    let Response::Prepared { image_handle, .. } = prepared else {
        panic!()
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("tcp://{}", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        let observer = socket.try_clone().unwrap();
        assert!(!observer.nodelay().unwrap());
        serve_connection(Stream::Tcp(socket), &cache, Some("secret"), None).unwrap();
        assert!(
            observer.nodelay().unwrap(),
            "server must enable TCP_NODELAY"
        );
    });
    let client = use_server_reuse(
        CacheClient::new(address, Some("secret".into())).unwrap(),
        true,
    );
    let error = client
        .request(Request::Stat {
            digest: image_handle.clone(),
            path: b"missing".to_vec(),
        })
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::NotFound
    );
    let (metadata, body) = client
        .request(Request::Stat {
            digest: image_handle.clone(),
            path: b"hello".to_vec(),
        })
        .unwrap();
    assert!(matches!(metadata, Response::Metadata { size: 11, .. }));
    assert!(body.is_empty());
    // The listener accepts only once: negative/positive Stat and the following
    // body-bearing reads must all use that same connection.
    let cases: [(u64, &[u8]); 3] = [(0, b"hello"), (6, b"world"), (11, b"")];
    for (offset, expected) in cases {
        let (_, bytes) = client
            .request(Request::Read {
                digest: image_handle.clone(),
                path: b"hello".to_vec(),
                offset,
                length: 5,
            })
            .unwrap();
        assert_eq!(bytes, expected);
        assert!(matches!(
            client.request(Request::Ping).unwrap().0,
            Response::Ready
        ));
    }
    drop(client);
    worker.join().unwrap();
}

#[test]
fn persistent_connections_recheck_token_and_refuse_version_changes() {
    for (version, token, code) in [
        (2, "wrong", "permission_denied"),
        (1, "secret", "unsupported_version"),
    ] {
        let (_tmp, store, _) = fixture();
        let cache = reader(&store).unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            serve_connection(Stream::Unix(server), &cache, Some("secret"), None).unwrap();
        });
        write_frame(
            &mut client,
            &Envelope {
                version: 2,
                token: Some("secret".into()),
                request: Request::Ping,
            },
        )
        .unwrap();
        assert!(matches!(
            read_frame::<Response>(&mut client).unwrap(),
            Response::Ready
        ));
        write_frame(
            &mut client,
            &Envelope {
                version,
                token: Some(token.into()),
                request: Request::Ping,
            },
        )
        .unwrap();
        assert!(
            matches!(read_frame::<Response>(&mut client).unwrap(), Response::Error { code: actual, .. } if actual == code)
        );
        worker.join().unwrap();
    }
}

#[test]
fn v2_preparation_queue_returns_result_without_handing_off_persistent_stream() {
    let (_tmp, store, _) = fixture();
    let cache = reader(&store).unwrap();
    let (queue, pending) = mpsc::sync_channel(1);
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let worker = std::thread::spawn(move || {
        serve_connection(Stream::Unix(server), &cache, None, Some(&queue)).unwrap();
    });
    write_frame(
        &mut client,
        &Envelope {
            version: 2,
            token: None,
            request: Request::Prepare {
                image: "fixture".into(),
                architecture: "amd64".into(),
                refresh: false,
            },
        },
    )
    .unwrap();
    let (reply, request) = pending.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(matches!(request, Request::Prepare { .. }));
    let PrepareReply::Result(send) = reply else {
        panic!("v2 must retain stream ownership")
    };
    send.send(Ok((Response::Ready, vec![]))).unwrap();
    assert!(matches!(
        read_frame::<Response>(&mut client).unwrap(),
        Response::Ready
    ));
    write_frame(
        &mut client,
        &Envelope {
            version: 2,
            token: None,
            request: Request::Ping,
        },
    )
    .unwrap();
    assert!(matches!(
        read_frame::<Response>(&mut client).unwrap(),
        Response::Ready
    ));
    drop(client);
    worker.join().unwrap();
}

#[test]
fn persistent_idle_connection_closes_stream() {
    let (_tmp, store, _) = fixture();
    let cache = reader(&store).unwrap();
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let worker = std::thread::spawn(move || {
        serve_connection(Stream::Unix(server), &cache, None, None).unwrap();
    });
    write_frame(
        &mut client,
        &Envelope {
            version: 2,
            token: None,
            request: Request::Ping,
        },
    )
    .unwrap();
    assert!(matches!(
        read_frame::<Response>(&mut client).unwrap(),
        Response::Ready
    ));
    // The worker must close after its idle timeout even while the client lives.
    assert!(read_frame::<Response>(&mut client).is_err());
    worker.join().unwrap();
}

#[test]
fn tcp_authorization_refusal_is_not_hidden_by_legacy_fallback() {
    let (_tmp, store, _) = fixture();
    let cache = reader(&store).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("tcp://{}", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        serve_connection(Stream::Tcp(socket), &cache, Some("secret"), None).unwrap();
    });
    let client = use_server_reuse(
        CacheClient::new(address, Some("wrong".into())).unwrap(),
        true,
    );
    let error = client.request(Request::Ping).unwrap_err();
    assert!(error.to_string().contains("cache authentication failed"));
    worker.join().unwrap();
}

#[test]
fn v1_still_closes_after_one_request() {
    let (_tmp, store, _) = fixture();
    let cache = reader(&store).unwrap();
    let (mut client, server) = UnixStream::pair().unwrap();
    write_frame(
        &mut client,
        &Envelope {
            version: 1,
            token: None,
            request: Request::Ping,
        },
    )
    .unwrap();
    serve_connection(Stream::Unix(server), &cache, None, None).unwrap();
    assert!(matches!(
        read_frame::<Response>(&mut client).unwrap(),
        Response::Ready
    ));
    assert!(read_frame::<Response>(&mut client).is_err());
}

fn published_fixture() -> (tempfile::TempDir, PortableCache, String) {
    let (tmp, store, digest) = fixture();
    let cache = reader(&store).unwrap();
    let image = crate::image::oci::PreparedImage {
        digest: digest.clone(),
        rootfs: store.root.join("rootfs-v3/sha256").join(&digest[7..]),
        env: Default::default(),
        entrypoint: vec![],
        cmd: vec![],
    };
    let (response, _) = cache.publish(&store, &image, "amd64", "fixture").unwrap();
    let Response::Prepared { image_handle, .. } = response else {
        panic!()
    };
    (tmp, cache, image_handle)
}

fn queued_handlers(cache: PortableCache) -> (ConnectionHandlers, Vec<std::thread::JoinHandle<()>>) {
    let store = Arc::new(cache);
    let file_store = store.clone();
    let (files, mut workers) =
        start_request_workers(FILE_WORKERS, move |request| file_store.request(request));
    let prepare_store = store.clone();
    let (prepare, prepare_workers) = start_request_workers(PREPARE_WORKERS, move |request| {
        prepare_store.request(request)
    });
    workers.extend(prepare_workers);
    (
        ConnectionHandlers {
            active: Arc::new(AtomicUsize::new(0)),
            store,
            token: Arc::new(None),
            files,
            prepare,
        },
        workers,
    )
}

fn queued_connection(handlers: &ConnectionHandlers) -> (UnixStream, std::thread::JoinHandle<()>) {
    let (client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let worker = handlers.dispatch(Stream::Unix(server)).unwrap();
    (client, worker)
}

fn send_v2(client: &mut UnixStream, request: Request) {
    write_frame(
        client,
        &Envelope {
            version: 2,
            token: None,
            request,
        },
    )
    .unwrap();
}

fn queued_ping(client: &mut UnixStream) {
    send_v2(client, Request::Ping);
    assert!(matches!(
        read_frame::<Response>(client).unwrap(),
        Response::Ready
    ));
}

fn finish_queued_service(
    handlers: ConnectionHandlers,
    connections: Vec<std::thread::JoinHandle<()>>,
    workers: Vec<std::thread::JoinHandle<()>>,
) {
    for connection in connections {
        connection.join().unwrap();
    }
    assert_eq!(handlers.active.load(Ordering::Acquire), 0);
    drop(handlers);
    for worker in workers {
        worker.join().unwrap();
    }
}

#[test]
fn more_than_sixteen_idle_connections_do_not_starve_newcomers_and_admission_is_bounded() {
    let (_tmp, cache, handle) = published_fixture();
    let (handlers, workers) = queued_handlers(cache);
    let mut clients = Vec::new();
    let mut connections = Vec::new();
    for _ in 0..32 {
        let (mut client, connection) = queued_connection(&handlers);
        queued_ping(&mut client);
        clients.push(client);
        connections.push(connection);
    }
    assert_eq!(handlers.active.load(Ordering::Acquire), 32);
    let started = std::time::Instant::now();
    let (mut newcomer, connection) = queued_connection(&handlers);
    queued_ping(&mut newcomer);
    send_v2(
        &mut newcomer,
        Request::Stat {
            digest: handle,
            path: b"hello".to_vec(),
        },
    );
    assert!(matches!(
        read_frame::<Response>(&mut newcomer).unwrap(),
        Response::Metadata { size: 11, .. }
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
    clients.push(newcomer);
    connections.push(connection);
    for _ in clients.len()..CONNECTION_LIMIT {
        let (mut client, connection) = queued_connection(&handlers);
        queued_ping(&mut client);
        clients.push(client);
        connections.push(connection);
    }
    assert_eq!(handlers.active.load(Ordering::Acquire), CONNECTION_LIMIT);
    let (mut excess, server) = UnixStream::pair().unwrap();
    excess
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    assert!(handlers.dispatch(Stream::Unix(server)).is_err());
    assert!(read_frame::<Response>(&mut excess).is_err());
    assert_eq!(handlers.active.load(Ordering::Acquire), CONNECTION_LIMIT);
    // Admission saturation does not consume request-worker capacity.
    queued_ping(&mut clients[0]);
    drop(clients);
    finish_queued_service(handlers, connections, workers);
}

#[test]
fn sixteen_blocked_v2_preparations_do_not_starve_ping_or_stat() {
    let (_tmp, cache, handle) = published_fixture();
    let store = Arc::new(cache);
    let file_store = store.clone();
    let (files, mut workers) =
        start_request_workers(FILE_WORKERS, move |request| file_store.request(request));
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let worker_gate = gate.clone();
    let (started, receive_started) = mpsc::sync_channel(REQUEST_QUEUE_LIMIT);
    let (prepare, prepare_workers) = start_request_workers(PREPARE_WORKERS, move |request| {
        assert!(matches!(request, Request::Prepare { .. }));
        let (lock, wake) = &*worker_gate;
        let mut open = lock.lock().unwrap();
        if !*open {
            started.send(()).unwrap();
        }
        while !*open {
            open = wake.wait(open).unwrap();
        }
        Ok((Response::Ready, vec![]))
    });
    workers.extend(prepare_workers);
    let handlers = ConnectionHandlers {
        active: Arc::new(AtomicUsize::new(0)),
        store,
        token: Arc::new(None),
        files,
        prepare,
    };
    let mut clients = Vec::new();
    let mut connections = Vec::new();
    // Establish all 16 persistent streams before blocking their requests. Idle
    // streams alone must not reserve any of the 16 file-service workers.
    for _ in 0..16 {
        let (mut client, connection) = queued_connection(&handlers);
        queued_ping(&mut client);
        clients.push(client);
        connections.push(connection);
    }
    for client in &mut clients {
        send_v2(
            client,
            Request::Prepare {
                image: "blocked".into(),
                architecture: "amd64".into(),
                refresh: true,
            },
        );
    }
    for _ in 0..PREPARE_WORKERS {
        receive_started
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
    }
    let started = std::time::Instant::now();
    let (mut newcomer, connection) = queued_connection(&handlers);
    queued_ping(&mut newcomer);
    send_v2(
        &mut newcomer,
        Request::Stat {
            digest: handle,
            path: b"hello".to_vec(),
        },
    );
    assert!(matches!(
        read_frame::<Response>(&mut newcomer).unwrap(),
        Response::Metadata { size: 11, .. }
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
    connections.push(connection);
    let (lock, wake) = &*gate;
    *lock.lock().unwrap() = true;
    wake.notify_all();
    for client in &mut clients {
        assert!(matches!(
            read_frame::<Response>(client).unwrap(),
            Response::Ready
        ));
        queued_ping(client);
    }
    drop(newcomer);
    drop(clients);
    finish_queued_service(handlers, connections, workers);
}

#[test]
fn full_queues_reply_busy_without_admitting_requests_and_preserve_ordering() {
    let (_tmp, cache, _) = published_fixture();
    let (files, pending_files) = mpsc::sync_channel(REQUEST_QUEUE_LIMIT);
    let (prepare, pending_prepare) = mpsc::sync_channel(REQUEST_QUEUE_LIMIT);
    for _ in 0..REQUEST_QUEUE_LIMIT {
        let (reply, _receive) = mpsc::sync_channel(1);
        files
            .try_send((PrepareReply::Result(reply), Request::Ping))
            .unwrap_or_else(|_| panic!("fill file queue"));
        let (reply, _receive) = mpsc::sync_channel(1);
        prepare
            .try_send((
                PrepareReply::Result(reply),
                Request::Prepare {
                    image: "filler".into(),
                    architecture: "amd64".into(),
                    refresh: false,
                },
            ))
            .unwrap_or_else(|_| panic!("fill prepare queue"));
    }
    let handlers = ConnectionHandlers {
        active: Arc::new(AtomicUsize::new(0)),
        store: Arc::new(cache),
        token: Arc::new(None),
        files,
        prepare,
    };
    let (mut client, connection) = queued_connection(&handlers);
    for request in [
        Request::Stat {
            digest: "unused".into(),
            path: b"file".to_vec(),
        },
        Request::Prepare {
            image: "must-not-run".into(),
            architecture: "amd64".into(),
            refresh: true,
        },
    ] {
        send_v2(&mut client, request);
        assert!(
            matches!(read_frame::<Response>(&mut client).unwrap(), Response::Error { message, .. } if message.contains("queue is busy"))
        );
    }
    for _ in 0..REQUEST_QUEUE_LIMIT {
        assert!(matches!(pending_files.try_recv().unwrap().1, Request::Ping));
        assert!(
            matches!(pending_prepare.try_recv().unwrap().1, Request::Prepare { image, .. } if image == "filler")
        );
    }
    assert!(matches!(
        pending_files.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(matches!(
        pending_prepare.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    // After queue capacity becomes available, the same connection can submit
    // its next ordered request. The rejected Prepare must never appear here.
    send_v2(&mut client, Request::Ping);
    let (reply, request) = pending_files.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(request, Request::Ping));
    let PrepareReply::Result(reply) = reply else {
        panic!()
    };
    reply.send(Ok((Response::Ready, vec![]))).unwrap();
    assert!(matches!(
        read_frame::<Response>(&mut client).unwrap(),
        Response::Ready
    ));
    drop(client);
    finish_queued_service(handlers, vec![connection], vec![]);
}

#[test]
fn legacy_preparation_handoff_keeps_admission_counted_until_reply() {
    let (_tmp, cache, _) = published_fixture();
    let (files, _pending_files) = mpsc::sync_channel(REQUEST_QUEUE_LIMIT);
    let (prepare, pending_prepare) = mpsc::sync_channel(REQUEST_QUEUE_LIMIT);
    let handlers = ConnectionHandlers {
        active: Arc::new(AtomicUsize::new(0)),
        store: Arc::new(cache),
        token: Arc::new(None),
        files,
        prepare,
    };
    let (mut client, connection) = queued_connection(&handlers);
    write_frame(
        &mut client,
        &Envelope {
            version: 1,
            token: None,
            request: Request::Prepare {
                image: "fixture".into(),
                architecture: "amd64".into(),
                refresh: false,
            },
        },
    )
    .unwrap();
    let (reply, request) = pending_prepare
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    assert!(matches!(request, Request::Prepare { .. }));
    connection.join().unwrap();
    assert_eq!(handlers.active.load(Ordering::Acquire), 1);
    let PrepareReply::Connection(stream, permit) = reply else {
        panic!()
    };
    reply_result(stream, Ok((Response::Ready, vec![]))).unwrap();
    drop(permit);
    assert!(matches!(
        read_frame::<Response>(&mut client).unwrap(),
        Response::Ready
    ));
    assert!(read_frame::<Response>(&mut client).is_err());
    assert_eq!(handlers.active.load(Ordering::Acquire), 0);
}

#[test]
fn request_execution_and_pending_queues_have_fixed_bounds() {
    for count in [FILE_WORKERS, PREPARE_WORKERS] {
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let worker_gate = gate.clone();
        let active = Arc::new(AtomicUsize::new(0));
        let worker_active = active.clone();
        let executed = Arc::new(AtomicUsize::new(0));
        let worker_executed = executed.clone();
        let (started, receive_started) = mpsc::sync_channel(count);
        let (queue, workers) = start_request_workers(count, move |_| {
            worker_active.fetch_add(1, Ordering::AcqRel);
            let (lock, wake) = &*worker_gate;
            let mut open = lock.lock().unwrap();
            if !*open {
                started.send(()).unwrap();
            }
            while !*open {
                open = wake.wait(open).unwrap();
            }
            worker_active.fetch_sub(1, Ordering::AcqRel);
            worker_executed.fetch_add(1, Ordering::AcqRel);
            Ok((Response::Ready, vec![]))
        });
        for _ in 0..count {
            let (reply, _receive) = mpsc::sync_channel(1);
            queue
                .send((PrepareReply::Result(reply), Request::Ping))
                .unwrap_or_else(|_| panic!("worker queue closed"));
        }
        for _ in 0..count {
            receive_started
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
        }
        assert_eq!(active.load(Ordering::Acquire), count);
        for _ in 0..REQUEST_QUEUE_LIMIT {
            let (reply, _receive) = mpsc::sync_channel(1);
            queue
                .try_send((PrepareReply::Result(reply), Request::Ping))
                .unwrap_or_else(|_| panic!("queue capacity smaller than limit"));
        }
        let (reply, _receive) = mpsc::sync_channel(1);
        assert!(matches!(
            queue.try_send((PrepareReply::Result(reply), Request::Ping)),
            Err(mpsc::TrySendError::Full(_))
        ));
        assert_eq!(active.load(Ordering::Acquire), count);
        let (lock, wake) = &*gate;
        *lock.lock().unwrap() = true;
        wake.notify_all();
        drop(queue);
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(active.load(Ordering::Acquire), 0);
        assert_eq!(
            executed.load(Ordering::Acquire),
            count + REQUEST_QUEUE_LIMIT
        );
    }
}
