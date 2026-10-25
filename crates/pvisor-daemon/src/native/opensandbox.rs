//! OpenSandbox 1.1.0 core wire profile. Unimplemented features fail explicitly.
use super::*;
use axum::extract::{FromRequest, Multipart};
use serde::Deserialize;

fn sse(value: Value) -> Vec<u8> {
    format!("data: {}\n\n", value).into_bytes()
}
#[derive(Default)]
struct Utf8(Vec<u8>);
impl Utf8 {
    fn text(&mut self, bytes: &[u8], finish: bool) -> String {
        self.0.extend_from_slice(bytes);
        let keep = if finish {
            0
        } else {
            match std::str::from_utf8(&self.0) {
                Err(error) if error.error_len().is_none() => self.0.len() - error.valid_up_to(),
                _ => 0,
            }
        };
        let length = self.0.len() - keep;
        let text = String::from_utf8_lossy(&self.0[..length]).into_owned();
        self.0.drain(..length);
        text
    }
}
pub(super) fn events(job: Arc<Job>, connect: bool, cancel: Option<Service>) -> Response {
    struct State {
        job: Arc<Job>,
        revision: watch::Receiver<u64>,
        cursor: usize,
        done: bool,
        text: [Utf8; 2],
        connect: bool,
        cancel: Option<Service>,
    }
    impl Drop for State {
        fn drop(&mut self) {
            if !self.job.done()
                && let Some(service) = self.cancel.take()
            {
                let job = self.job.clone();
                tokio::spawn(async move {
                    let _ = service.signal(&job, libc::SIGKILL).await;
                });
            }
        }
    }
    let revision = job.changed.subscribe();
    let state = State {
        job,
        revision,
        cursor: 0,
        done: false,
        text: Default::default(),
        connect,
        cancel,
    };
    let stream = futures_util::stream::unfold(state, |mut state| async move {
        if state.done {
            return None;
        }
        loop {
            let event = state
                .job
                .journal
                .lock()
                .unwrap()
                .events
                .get(state.cursor)
                .cloned();
            if let Some(event) = event {
                state.cursor += 1;
                let mut output = Vec::new();
                if state.connect {
                    use base64::Engine;
                    let event = match event {
                        Event::Start => json!({"start":{"pid":state.job.pid}}),
                        Event::Output(channel, bytes) => {
                            json!({"data":{if state.job.launch.tty.is_some() {"pty"} else if channel == 2 {"stderr"} else {"stdout"}:base64::engine::general_purpose::STANDARD.encode(bytes)}})
                        }
                        Event::Exit(code, error) => {
                            state.done = true;
                            json!({"end":{"exitCode":code,"exited":true,"status":if code == 0 {"exited"} else {"error"},"error":error}})
                        }
                    };
                    output.extend(super::e2b::envelope(0, &json!({"event":event})));
                    if state.done {
                        output.extend(super::e2b::envelope(2, &json!({})));
                    }
                } else {
                    match event {
                        Event::Start => output.extend(sse(
                            json!({"type":"init","text":state.job.id,"timestamp":timestamp()}),
                        )),
                        Event::Output(channel, bytes) => {
                            let text = state.text[usize::from(channel == 2)].text(&bytes, false);
                            if text.is_empty() {
                                continue;
                            }
                            output.extend(sse(json!({"type":if channel == 2 {"stderr"} else {"stdout"},"text":text,"timestamp":timestamp()})));
                        }
                        Event::Exit(code, error) => {
                            for (index, text) in state.text.iter_mut().enumerate() {
                                let text = text.text(&[], true);
                                if !text.is_empty() {
                                    output.extend(sse(json!({"type":if index == 0 {"stdout"} else {"stderr"},"text":text,"timestamp":timestamp()})));
                                }
                            }
                            if code != 0 {
                                output.extend(sse(json!({"type":"error","error":{"ename":if error.is_some() {"RuntimeError"} else {"CommandExitError"},"evalue":error.unwrap_or_else(||code.to_string()),"traceback":[]},"timestamp":timestamp()})));
                            }
                            output.extend(sse(json!({"type":"execution_complete","execution_time":(chrono::Utc::now()-state.job.started).num_milliseconds(),"timestamp":timestamp()})));
                            state.done = true;
                        }
                    }
                }
                return Some((Ok::<_, std::io::Error>(Bytes::from(output)), state));
            }
            if tokio::time::timeout(Duration::from_secs(15), state.revision.changed())
                .await
                .is_err()
            {
                let bytes = if state.connect {
                    super::e2b::envelope(0, &json!({"event":{"keepalive":{}}}))
                } else {
                    sse(json!({"type":"ping","timestamp":timestamp()}))
                };
                return Some((Ok(Bytes::from(bytes)), state));
            }
        }
    });
    (
        [
            (
                header::CONTENT_TYPE,
                if connect {
                    "application/connect+json"
                } else {
                    "text/event-stream"
                },
            ),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    command: Option<String>,
    argv: Option<Vec<String>>,
    cwd: Option<String>,
    #[serde(default)]
    envs: BTreeMap<String, String>,
    #[serde(default)]
    background: bool,
    timeout: Option<u64>,
    uid: Option<u32>,
    gid: Option<u32>,
}
fn command(body: Value) -> Result<(ExecLaunch, bool, u64), Error> {
    let body: Command = serde_json::from_value(body).map_err(|e| invalid(e.to_string()))?;
    if body.gid.is_some() && body.uid.is_none() {
        return Err(invalid("uid required with gid"));
    }
    let argv = match (body.command, body.argv) {
        (Some(command), None) if !command.trim().is_empty() => {
            vec!["/bin/bash".into(), "-c".into(), command]
        }
        (None, Some(argv)) if !argv.is_empty() && !argv[0].is_empty() => argv,
        _ => return Err(invalid("exactly one nonempty command or argv required")),
    };
    let cwd = body
        .cwd
        .filter(|value| !value.is_empty())
        .map(|p| guest_path(&p, false))
        .transpose()?;
    let user = body.uid.map(|uid| {
        body.gid
            .map_or_else(|| uid.to_string(), |gid| format!("{uid}:{gid}"))
    });
    Ok((
        ExecLaunch {
            argv,
            env: body.envs,
            cwd,
            user,
            tty: None,
        },
        body.background,
        body.timeout.unwrap_or(0),
    ))
}
pub(super) fn file_info(value: &Value) -> Value {
    let mode = value["mode"].as_u64().unwrap_or(0);
    let decimal = format!("{mode:o}").parse::<u64>().unwrap_or(0);
    let timestamp = |field: &str| {
        chrono::DateTime::from_timestamp(value[field].as_i64().unwrap_or(0), 0)
            .unwrap_or_default()
            .to_rfc3339()
    };
    // OpenSandbox's Linux profile calls inode change time "created_at".
    json!({"path":value["path"],"type":if value["type"] == "dir" {"directory"} else {"file"},"size":value["size"],"mode":decimal,"owner":value["owner"],"group":value["group"],"modified_at":timestamp("modified"),"created_at":timestamp("changed")})
}
fn permission(value: &Value, default: u32) -> Result<u32, Error> {
    for key in ["owner", "group"] {
        if value[key]
            .as_str()
            .is_some_and(|v| !v.is_empty() && v != "root" && v != "0")
        {
            return Err(unsupported("native file ownership changes are unsupported"));
        }
    }
    let Some(value) = value.get("mode") else {
        return Ok(default);
    };
    let value = value
        .as_u64()
        .ok_or_else(|| invalid("invalid octal permission"))?;
    let mode = u32::from_str_radix(&value.to_string(), 8)
        .map_err(|_| invalid("invalid octal permission"))?;
    if mode > 0o7777 {
        return Err(invalid("permission exceeds Unix mode bits"));
    }
    Ok(mode)
}
pub(super) async fn multipart(
    service: &Service,
    request: Request,
    e2b: bool,
) -> Result<Value, Error> {
    let target = query(&request, "path");
    if query(&request, "username").is_some_and(|u| u != "root" && u != "0") {
        return Err(unsupported("native file API currently supports root only"));
    }
    if request
        .headers()
        .get(header::CONTENT_ENCODING)
        .is_some_and(|value| value.as_bytes() != b"identity")
        || request
            .headers()
            .keys()
            .any(|k| k.as_str().starts_with("x-e2b-metadata"))
    {
        return Err(unsupported(
            "compressed uploads and xattr metadata are unsupported",
        ));
    }
    if request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|s| s.starts_with("application/octet-stream"))
        && e2b
    {
        use futures_util::StreamExt;
        let path = guest_path(&target.ok_or_else(|| invalid("path required"))?, true)?;
        let mut stream = service.upload_begin(path, 0o644).await?;
        let mut body = request.into_body().into_data_stream();
        let mut total = 0;
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|_| invalid("invalid upload body"))?;
            total += chunk.len();
            if total > 64 * 1024 * 1024 {
                return Err(Error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "native upload exceeds 64 MiB".into(),
                ));
            }
            upload_chunk(&mut stream, &chunk).await?;
        }
        return Ok(json!([Service::upload_end(&mut stream).await?]));
    }
    let mut form = Multipart::from_request(request, &())
        .await
        .map_err(|e| invalid(e.to_string()))?;
    let mut metadata = VecDeque::new();
    let mut results = Vec::new();
    let mut total = 0usize;
    while let Some(mut field) = form
        .next_field()
        .await
        .map_err(|e| invalid(e.to_string()))?
    {
        let name = field.name().unwrap_or("").to_owned();
        if !e2b && name == "metadata" {
            let mut bytes = Vec::new();
            while let Some(chunk) = field.chunk().await.map_err(|e| invalid(e.to_string()))? {
                if bytes.len() + chunk.len() > 16384 {
                    return Err(invalid("upload metadata exceeds limit"));
                }
                bytes.extend_from_slice(&chunk);
            }
            if metadata.len() >= 128 {
                return Err(invalid("too many upload entries"));
            }
            metadata.push_back(
                serde_json::from_slice::<Value>(&bytes).map_err(|e| invalid(e.to_string()))?,
            );
            continue;
        }
        if name != "file" {
            return Err(invalid("unknown multipart field"));
        }
        if results.len() >= 128 {
            return Err(invalid("too many upload entries"));
        }
        let meta = if e2b {
            json!({"path": target.clone().or_else(|| field.file_name().map(str::to_owned)).ok_or_else(||invalid("filename required"))?})
        } else {
            metadata
                .pop_front()
                .ok_or_else(|| invalid("metadata must precede file"))?
        };
        let path = guest_path(
            meta["path"]
                .as_str()
                .ok_or_else(|| invalid("file path required"))?,
            e2b,
        )?;
        let mut stream = service
            .upload_begin(path, permission(&meta, 0o644)?)
            .await?;
        while let Some(chunk) = field.chunk().await.map_err(|e| invalid(e.to_string()))? {
            total += chunk.len();
            if total > 64 * 1024 * 1024 {
                return Err(Error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "native upload exceeds 64 MiB".into(),
                ));
            }
            upload_chunk(&mut stream, &chunk).await?;
        }
        results.push(Service::upload_end(&mut stream).await?);
    }
    if !metadata.is_empty() || results.is_empty() {
        return Err(invalid("missing upload content"));
    }
    Ok(json!(results))
}
async fn upload_chunk(stream: &mut UnixStream, bytes: &[u8]) -> Result<(), Error> {
    for chunk in bytes.chunks(16384) {
        write(
            stream,
            &StdinFrame::Data {
                bytes: chunk.to_vec(),
            },
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn handle(service: Service, request: Request) -> Result<Response, Error> {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    match (method.as_str(), path.as_str()) {
        ("GET", "/ping") => {
            service.info().await?;
            Ok(axum::Json(json!({"status":"ok"})).into_response())
        }
        ("GET", "/ready") => {
            service.info().await?;
            Ok(axum::Json(json!({"initialized":true,"sandboxId":service.sandbox_id,"generation":service.generation_number})).into_response())
        }
        ("GET", "/code/contexts") => {
            service.info().await?;
            Ok(axum::Json(json!([])).into_response())
        }
        ("GET", "/metrics") => {
            let mut value = service.info().await?;
            // CPU utilization needs a measured interval, rather than a fabricated zero.
            let first = service.cpu_ticks().await?;
            tokio::time::sleep(Duration::from_millis(50)).await;
            let second = service.cpu_ticks().await?;
            value["cpu_used_pct"] = json!(
                second.0.saturating_sub(first.0) as f64
                    / second.1.saturating_sub(first.1).max(1) as f64
                    * 100.0
            );
            value["timestamp"] = json!(timestamp());
            Ok(axum::Json(value).into_response())
        }
        ("POST", "/command") => {
            let (launch, background, timeout_ms) = command(json_body(request).await?)?;
            let job = service.start(launch, false, None).await?;
            if timeout_ms > 0 {
                let current = job.clone();
                let service = service.clone();
                tokio::spawn(async move {
                    let mut changed = current.changed.subscribe();
                    let wait = async {
                        while !current.done() {
                            if changed.changed().await.is_err() {
                                break;
                            }
                        }
                    };
                    if tokio::time::timeout(Duration::from_millis(timeout_ms), wait)
                        .await
                        .is_err()
                    {
                        let _ = service.signal(&current, libc::SIGKILL).await;
                    }
                });
            }
            if background {
                let mut bytes = sse(json!({"type":"init","text":job.id,"timestamp":timestamp()}));
                bytes.extend(sse(
                    json!({"type":"execution_complete","timestamp":timestamp()}),
                ));
                Ok(([(header::CONTENT_TYPE, "text/event-stream")], bytes).into_response())
            } else {
                Ok(events(job, false, Some(service)))
            }
        }
        ("DELETE", "/command") => {
            let id = query(&request, "id").ok_or_else(|| invalid("id required"))?;
            let job = service.job(&json!(id))?;
            service.signal(&job, libc::SIGKILL).await?;
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        ("GET", path) if path.starts_with("/command/status/") => {
            let job = service.job(&json!(path.trim_start_matches("/command/status/")))?;
            let journal = job.journal.lock().unwrap();
            let code = journal.events.iter().find_map(|e| {
                if let Event::Exit(code, _) = e {
                    Some(*code)
                } else {
                    None
                }
            });
            Ok(axum::Json(json!({"id":job.id,"running":!journal.done,"exit_code":code,"started_at":job.started})).into_response())
        }
        ("GET", path) if path.starts_with("/command/") && path.ends_with("/logs") => {
            let id = path
                .trim_start_matches("/command/")
                .trim_end_matches("/logs");
            let cursor = query(&request, "cursor")
                .map(|v| {
                    v.parse::<usize>()
                        .map_err(|_| invalid("invalid log cursor"))
                })
                .transpose()?
                .unwrap_or(0);
            let job = service.job(&json!(id))?;
            let journal = job.journal.lock().unwrap();
            let bytes: Vec<u8> = journal
                .events
                .iter()
                .filter_map(|event| {
                    if let Event::Output(_, bytes) = event {
                        Some(bytes.as_slice())
                    } else {
                        None
                    }
                })
                .flatten()
                .copied()
                .collect();
            if cursor > bytes.len() {
                return Err(invalid("log cursor exceeds output"));
            }
            Ok((
                [
                    (
                        header::CONTENT_TYPE,
                        "text/plain; charset=utf-8"
                            .parse::<axum::http::HeaderValue>()
                            .unwrap(),
                    ),
                    (
                        header::HeaderName::from_static("execd-commands-tail-cursor"),
                        bytes
                            .len()
                            .to_string()
                            .parse::<axum::http::HeaderValue>()
                            .unwrap(),
                    ),
                ],
                bytes[cursor..].to_vec(),
            )
                .into_response())
        }
        ("POST", "/files/upload") => {
            multipart(&service, request, false).await?;
            Ok(StatusCode::OK.into_response())
        }
        ("GET", "/files/download") => {
            let path = query(&request, "path").ok_or_else(|| invalid("path required"))?;
            if ["offset", "limit"]
                .iter()
                .any(|key| query(&request, key).is_some_and(|v| !v.is_empty()))
            {
                return Err(unsupported(
                    "native line-based reads are unsupported; use byte ranges",
                ));
            }
            let range = request
                .headers()
                .get(header::RANGE)
                .map(|value| value.to_str().map_err(|_| invalid("invalid Range header")))
                .transpose()?;
            service.download(guest_path(&path, false)?, range).await
        }
        ("GET", "/files/info") => {
            let mut result = serde_json::Map::new();
            for (key, path) in
                url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
            {
                if key == "path" {
                    result.insert(
                        path.to_string(),
                        file_info(
                            &service
                                .file(FileRequest::Stat {
                                    path: guest_path(&path, false)?,
                                })
                                .await?,
                        ),
                    );
                }
            }
            Ok(axum::Json(result).into_response())
        }
        ("GET", "/directories/list") => {
            if let Some(depth) = query(&request, "depth")
                && depth
                    .parse::<u64>()
                    .map_err(|_| invalid("invalid directory depth"))?
                    != 1
            {
                return Err(unsupported("native directory listing supports depth 1"));
            }
            let path = query(&request, "path").ok_or_else(|| invalid("path required"))?;
            let values = service
                .file(FileRequest::List {
                    path: guest_path(&path, false)?,
                })
                .await?;
            Ok(axum::Json(
                values
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(file_info)
                    .collect::<Vec<_>>(),
            )
            .into_response())
        }
        ("POST", "/directories" | "/files/permissions") => {
            let directory = path == "/directories";
            let body = json_body(request).await?;
            for (path, value) in body
                .as_object()
                .ok_or_else(|| invalid("expected path map"))?
            {
                let path = guest_path(path, false)?;
                let mode = permission(value, if directory { 0o755 } else { 0o644 })?;
                service
                    .file(if directory {
                        FileRequest::MakeDir { path, mode }
                    } else {
                        FileRequest::Chmod { path, mode }
                    })
                    .await?;
            }
            Ok(StatusCode::OK.into_response())
        }
        ("DELETE", "/files" | "/directories") => {
            for (key, path) in
                url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
            {
                if key == "path" {
                    service
                        .file(FileRequest::Remove {
                            path: guest_path(&path, false)?,
                        })
                        .await?;
                }
            }
            Ok(StatusCode::OK.into_response())
        }
        ("POST", "/files/mv") => {
            let body = json_body(request).await?;
            for item in body
                .as_array()
                .ok_or_else(|| invalid("expected moves array"))?
            {
                service
                    .file(FileRequest::Move {
                        source: guest_path(
                            item["src"]
                                .as_str()
                                .ok_or_else(|| invalid("src required"))?,
                            false,
                        )?,
                        destination: guest_path(
                            item["dest"]
                                .as_str()
                                .ok_or_else(|| invalid("dest required"))?,
                            false,
                        )?,
                    })
                    .await?;
            }
            Ok(StatusCode::OK.into_response())
        }
        _ => Err(unsupported(
            "operation is not supported by the native OpenSandbox core profile",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_info_contains_required_sdk_timestamps_and_guest_permissions() {
        let info = file_info(&json!({
            "path":"/tmp/file ✓", "type":"file", "size":8193, "mode":0o640,
            "owner":"root", "group":"root", "modified":100, "changed":110,
        }));
        assert_eq!(info["path"], "/tmp/file ✓");
        assert_eq!(info["size"], 8193);
        assert_eq!(info["mode"], 640);
        assert_eq!(info["type"], "file");
        for (field, expected) in [("modified_at", 100), ("created_at", 110)] {
            let text = info[field]
                .as_str()
                .expect("SDK requires an RFC3339 timestamp");
            assert_eq!(
                chrono::DateTime::parse_from_rfc3339(text)
                    .unwrap()
                    .timestamp(),
                expected
            );
        }
        let directory = file_info(&json!({"type":"dir","mode":0o755,"modified":0,"changed":0}));
        assert_eq!(directory["type"], "directory");
    }

    #[test]
    fn split_utf8_and_literal_argv_survive_protocol_translation() {
        let mut text = Utf8::default();
        let bytes = "雪✓".as_bytes();
        assert_eq!(text.text(&bytes[..2], false), "");
        assert_eq!(text.text(&bytes[2..4], false), "雪");
        assert_eq!(text.text(&bytes[4..], false), "✓");
        assert_eq!(text.text(&[0xe9], true), "�");
        let (launch, _, _) =
            command(json!({"argv":["/tool","$HOME",";",""],"envs":{"VALUE":"$HOME"}})).unwrap();
        assert_eq!(launch.argv, ["/tool", "$HOME", ";", ""]);
        assert_eq!(launch.env["VALUE"], "$HOME");
        assert!(command(json!({"command":"x","argv":["y"]})).is_err());
        assert!(command(json!({"command":"x","unknown":true})).is_err());
        assert!(command(json!({"command":"x","cwd":"relative"})).is_err());
        assert_eq!(permission(&json!({"mode":644}), 0).unwrap(), 0o644);
        assert_eq!(permission(&json!({"mode":0}), 0o644).unwrap(), 0);
        assert_eq!(permission(&json!({}), 0o640).unwrap(), 0o640);
        for value in [
            json!({"mode":-1}),
            json!({"mode":"644"}),
            json!({"mode":888}),
            json!({"mode":10000}),
        ] {
            assert!(permission(&value, 0o644).is_err(), "{value}");
        }
        assert!(permission(&json!({"owner":"someone"}), 0o644).is_err());
    }
}
