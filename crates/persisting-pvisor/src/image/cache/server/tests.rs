//! Service confinement, protocol, and client/server regression tests.
use super::*;
use crate::image::cache::CacheClient;
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
fn server_metadata_reuses_directory_index_and_invalidates_rebuilt_root() {
    let (_tmp, store, digest) = fixture();
    let first = metadata::directory(&store, &digest, b"").unwrap();
    let again = metadata::directory(&store, &digest, b"").unwrap();
    assert!(
        Arc::ptr_eq(&first, &again),
        "directory must not be scanned again"
    );
    let Response::Metadata { size, .. } = metadata::stat(&store, &digest, b"hello").unwrap() else {
        panic!()
    };
    assert_eq!(size, 11);
    let generation = metadata::generation(&store, &digest).unwrap();
    let root = store.root.join("rootfs-v3/sha256").join(&digest[7..]);
    fs::rename(&root, root.with_extension("old")).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("hello"), b"new").unwrap();
    assert_ne!(generation, metadata::generation(&store, &digest).unwrap());
    assert_eq!(
        &*metadata::directory(&store, &digest, b"").unwrap(),
        &[b"hello".to_vec()]
    );
    let Response::Metadata { size, .. } = metadata::stat(&store, &digest, b"hello").unwrap() else {
        panic!()
    };
    assert_eq!(size, 3);
}

#[test]
fn prepare_queue_is_bounded_and_does_not_block_ping() {
    let (_tmp, store, _) = fixture();
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
fn paths_metadata_pagination_and_eof() {
    let (_tmp, store, digest) = fixture();
    let (response, _) = handle(
        &store,
        Request::List {
            digest: digest.clone(),
            path: vec![],
            offset: 0,
        },
    )
    .unwrap();
    match response {
        Response::Entries {
            names,
            metadata,
            next_offset,
        } => {
            assert_eq!(metadata.as_ref().unwrap().len(), names.len());
            assert_eq!(
                names,
                [
                    b"alias".to_vec(),
                    b"dir".to_vec(),
                    b"escape".to_vec(),
                    b"hello".to_vec()
                ]
            );
            assert!(next_offset.is_none());
        }
        _ => panic!("expected directory"),
    }
    let (response, _) = handle(
        &store,
        Request::Stat {
            digest: digest.clone(),
            path: b"alias".to_vec(),
        },
    )
    .unwrap();
    assert!(
        matches!(response, Response::Metadata { target: Some(target), .. } if target == b"hello")
    );
    for path in [
        b"../hello".as_slice(),
        b"/etc/passwd",
        b"escape/passwd",
        b"alias",
        b"hello\0",
    ] {
        assert!(
            handle(
                &store,
                Request::Read {
                    digest: digest.clone(),
                    path: path.to_vec(),
                    offset: 0,
                    length: 10
                }
            )
            .is_err()
        );
    }
    assert!(
        handle(
            &store,
            Request::Read {
                digest: "sha256:../../etc".into(),
                path: b"passwd".to_vec(),
                offset: 0,
                length: 10
            }
        )
        .is_err()
    );
    assert!(
        handle(
            &store,
            Request::Read {
                digest: digest.clone(),
                path: b"hello".to_vec(),
                offset: 0,
                length: MAX_READ + 1
            }
        )
        .is_err()
    );
    let (_, body) = handle(
        &store,
        Request::Read {
            digest,
            path: b"hello".to_vec(),
            offset: 100,
            length: 10,
        },
    )
    .unwrap();
    assert!(body.is_empty());
}

#[test]
fn directory_metadata_pages_fit_frames_and_old_pages_still_decode() {
    use std::os::unix::ffi::OsStringExt;
    let (_tmp, store, digest) = fixture();
    let root = store.root.join("rootfs-v3/sha256").join(&digest[7..]);
    let target = std::ffi::OsString::from_vec(vec![b'~'; 1000]);
    for index in 0..256 {
        let mut name = format!("{index:03}-").into_bytes();
        name.extend(vec![b'~'; 240]);
        std::os::unix::fs::symlink(&target, root.join(std::ffi::OsString::from_vec(name))).unwrap();
    }
    let mut offset = 0;
    let mut pages = 0;
    loop {
        let (response, _) = handle(
            &store,
            Request::List {
                digest: digest.clone(),
                path: vec![],
                offset,
            },
        )
        .unwrap();
        let mut frame = Vec::new();
        write_frame(&mut frame, &response).unwrap();
        assert!(frame.len() <= MAX_FRAME + 4);
        let Response::Entries {
            names,
            metadata,
            next_offset,
        } = response
        else {
            panic!()
        };
        assert_eq!(metadata.unwrap().len(), names.len());
        assert!(!names.is_empty());
        offset += names.len();
        pages += 1;
        if let Some(next) = next_offset {
            assert_eq!(next, offset);
        } else {
            break;
        }
    }
    assert_eq!(offset, 260);
    assert!(pages > 1);
    let old: Response =
        serde_json::from_str(r#"{"status":"entries","names":[[97]],"next_offset":null}"#).unwrap();
    assert!(matches!(old, Response::Entries { metadata: None, .. }));
}

#[test]
fn rejects_bad_frames_versions_and_tokens() {
    assert!(read_frame::<Envelope>(&mut &u32::MAX.to_be_bytes()[..]).is_err());
    assert!(endpoint("tcp://0.0.0.0:9000").is_err());
    assert!(CacheClient::new("tcp://127.0.0.1:9000".into(), None).is_err());
    for (version, token) in [(2, Some("secret")), (1, Some("wrong")), (1, None)] {
        let (_tmp, store, digest) = fixture();
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
