use super::*;
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, Uri},
    response::{IntoResponse, Response},
    routing::any,
};
use std::{collections::BTreeMap, sync::Mutex};

#[derive(Default)]
struct Bucket {
    endpoint: String,
    object: Option<(u32, Vec<u8>)>,
    sessions: Vec<u32>,
    overlap: Option<Arc<tokio::sync::Barrier>>,
    ranges: Vec<String>,
    ignore_range: bool,
}

fn metadata(generation: u32, bytes: &[u8]) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::json!({
            "size": bytes.len().to_string(), "generation": generation.to_string(), "etag": "opaque",
        })
        .to_string(),
    )
        .into_response()
}

async fn gcs_request(
    State(bucket): State<Arc<Mutex<Bucket>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let args: BTreeMap<_, _> = uri
        .query()
        .unwrap_or("")
        .split('&')
        .filter_map(|p| p.split_once('='))
        .collect();
    if method == Method::POST && uri.path() == "/upload/storage/v1/b/unit-bucket/o" {
        assert_eq!(headers[header::AUTHORIZATION], "Bearer loopback-test-token");
        assert_eq!(args["uploadType"], "resumable");
        assert_eq!(args["name"], "prefix%2FHEAD.json");
        let expected = args["ifGenerationMatch"].parse::<u32>().unwrap();
        let (location, gate) = {
            let mut state = bucket.lock().unwrap();
            let generation = state
                .object
                .as_ref()
                .map_or(0, |(generation, _)| *generation);
            if expected != generation {
                return StatusCode::PRECONDITION_FAILED.into_response();
            }
            let session = state.sessions.len();
            state.sessions.push(expected);
            (
                format!("{}/session/{session}", state.endpoint),
                state.overlap.clone(),
            )
        };
        if let Some(gate) = gate {
            tokio::time::timeout(Duration::from_secs(5), gate.wait())
                .await
                .unwrap();
        }
        return ([(header::LOCATION, location)], "").into_response();
    }
    let mut state = bucket.lock().unwrap();
    let generation = state
        .object
        .as_ref()
        .map_or(0, |(generation, _)| *generation);
    if method == Method::PUT
        && let Some(session) = uri.path().strip_prefix("/session/")
    {
        if state.sessions[session.parse::<usize>().unwrap()] != generation {
            return StatusCode::PRECONDITION_FAILED.into_response();
        }
        state.object = Some((generation + 1, body.to_vec()));
        return metadata(generation + 1, &body);
    }
    assert_eq!(method, Method::GET);
    assert_eq!(headers[header::AUTHORIZATION], "Bearer loopback-test-token");
    assert!(
        uri.path()
            .starts_with("/storage/v1/b/unit-bucket/o/prefix%2F")
    );
    let Some((generation, bytes)) = state.object.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if args.get("alt") != Some(&"media") {
        return metadata(generation, &bytes);
    }
    if let Some(expected) = args.get("ifGenerationMatch")
        && expected.parse::<u32>().unwrap() != generation
    {
        return StatusCode::PRECONDITION_FAILED.into_response();
    }
    if let Some(value) = headers.get(header::RANGE) {
        let value = value.to_str().unwrap();
        state.ranges.push(value.into());
        if state.ignore_range {
            return bytes.into_response();
        }
        let (start, end) = value
            .strip_prefix("bytes=")
            .unwrap()
            .split_once('-')
            .unwrap();
        let (start, end): (usize, usize) = (start.parse().unwrap(), end.parse().unwrap());
        return (
            StatusCode::PARTIAL_CONTENT,
            [(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{}", bytes.len()),
            )],
            bytes[start..=end].to_vec(),
        )
            .into_response();
    }
    bytes.into_response()
}

#[test]
fn gcs_generation_cas_and_bounded_ranges_through_opendal() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let bucket = Arc::new(Mutex::new(Bucket {
        endpoint: endpoint.clone(),
        ..Default::default()
    }));
    let app = Router::new()
        .fallback(any(gcs_request))
        .with_state(bucket.clone());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let worker = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                axum::serve(tokio::net::TcpListener::from_std(listener).unwrap(), app)
                    .with_graceful_shutdown(async {
                        let _ = stopped.await;
                    })
                    .await
                    .unwrap();
            })
    });
    let storage = Storage::OpenDal(Arc::new(
        Remote::new(
            operator(
                Gcs::default()
                    .endpoint(&endpoint)
                    .bucket("unit-bucket")
                    .root("prefix")
                    .token("loopback-test-token".into()),
            )
            .unwrap(),
            false,
        )
        .unwrap(),
    ));
    let key = "HEAD.json";
    assert!(storage.get(key).unwrap().is_none());
    storage
        .compare_and_swap(key, b"before".to_vec(), None)
        .unwrap();
    let old = storage.get_versioned(key).unwrap().unwrap();
    assert_eq!(old.bytes, b"before");
    assert_eq!(old.version.version.as_deref(), Some("1"));
    assert!(super::super::storage::is_conflict(
        &storage.compare_and_swap(key, vec![], None).unwrap_err()
    ));
    storage
        .compare_and_swap(key, b"after".to_vec(), Some(old.version.clone()))
        .unwrap();
    assert!(super::super::storage::is_conflict(
        &storage
            .compare_and_swap(key, vec![], Some(old.version))
            .unwrap_err()
    ));
    assert_eq!(storage.range(key, 1..3).unwrap(), b"ft");
    assert_eq!(bucket.lock().unwrap().ranges.last().unwrap(), "bytes=1-2");
    assert!(storage.read(key, None, 4).is_err());
    bucket.lock().unwrap().ignore_range = true;
    assert!(storage.range(key, 1..3).is_err());
    let generation = storage.get_versioned(key).err();
    assert!(
        generation.is_some(),
        "a full read must also reject an incorrect Range response"
    );
    bucket.lock().unwrap().ignore_range = false;
    let version = storage.get_versioned(key).unwrap().unwrap().version;
    bucket.lock().unwrap().overlap = Some(Arc::new(tokio::sync::Barrier::new(2)));
    let update =
        |bytes: &[u8]| storage.compare_and_swap(key, bytes.to_vec(), Some(version.clone()));
    std::thread::scope(|scope| {
        let left = scope.spawn(|| update(b"left"));
        let right = update(b"right");
        let left = left.join().unwrap();
        match (left, right) {
            (Ok(()), Err(conflict)) => {
                assert!(super::super::storage::is_conflict(&conflict));
                assert_eq!(storage.get(key).unwrap().unwrap(), b"left");
            }
            (Err(conflict), Ok(())) => {
                assert!(super::super::storage::is_conflict(&conflict));
                assert_eq!(storage.get(key).unwrap().unwrap(), b"right");
            }
            result => panic!("expected exactly one successful conditional write: {result:?}"),
        }
    });
    stop.send(()).unwrap();
    worker.join().unwrap();
}
