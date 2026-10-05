//! Service confinement, protocol, and client/server regression tests.
use super::*;
use crate::image::cache::CacheClient;
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
        image_handle: Some(digest),
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
                let client = CacheClient::new(address, None).unwrap();
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
    for (version, token) in [(2, Some("secret")), (1, Some("wrong")), (1, None)] {
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
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("server.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let worker = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let _: Envelope = read_frame(&mut socket).unwrap();
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
    let client = CacheClient::new(format!("unix://{}", path.display()), None).unwrap();
    let error = client
        .request(Request::Read {
            digest: "unused".into(),
            path: b"file".to_vec(),
            offset: 0,
            length: 3,
        })
        .unwrap_err();
    assert!(error.to_string().contains("digest mismatch"));
    worker.join().unwrap();
}

#[test]
fn server_and_direct_readers_share_revisions_after_source_removal() {
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
        image_handle: Some(handle),
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
    let count = requests.len() + 1;
    let server = std::thread::spawn(move || {
        for connection in listener.incoming().take(count) {
            serve_connection(Stream::Unix(connection.unwrap()), &cache, None, None).unwrap();
        }
    });
    let client = CacheClient::new(format!("unix://{}", socket.display()), None).unwrap();
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
    server.join().unwrap();
}
