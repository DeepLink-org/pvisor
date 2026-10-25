//! E2B envd 0.10.2 JSON/Connect core profile (the pinned SDK's wire format).
use super::*;
use base64::Engine;

pub(super) fn envelope(flags: u8, value: &Value) -> Vec<u8> {
    let bytes = serde_json::to_vec(value).expect("JSON value");
    let mut frame = Vec::with_capacity(bytes.len() + 5);
    frame.push(flags);
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend(bytes);
    frame
}
fn decode(bytes: &[u8]) -> Result<Value, Error> {
    if bytes.len() < 5 || bytes[0] != 0 {
        return Err(invalid("expected one uncompressed Connect JSON message"));
    }
    let length = u32::from_be_bytes(bytes[1..5].try_into().unwrap()) as usize;
    if length > MAX_FRAME || bytes.len() != length + 5 {
        return Err(invalid("invalid Connect message length"));
    }
    serde_json::from_slice(&bytes[5..]).map_err(|e| invalid(e.to_string()))
}
fn username(request: &Request) -> Result<Option<String>, Error> {
    let Some(header) = request.headers().get(header::AUTHORIZATION) else {
        return Ok(None);
    };
    let encoded = header
        .to_str()
        .ok()
        .and_then(|s| s.strip_prefix("Basic "))
        .ok_or_else(|| invalid("Basic guest user required"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| invalid("invalid Basic identity"))?;
    let text = String::from_utf8(bytes).map_err(|_| invalid("invalid guest identity"))?;
    let user = text
        .split_once(':')
        .ok_or_else(|| invalid("invalid guest identity"))?
        .0;
    if user.is_empty() {
        return Err(invalid("empty guest identity"));
    }
    Ok(Some(user.into()))
}
fn entry(value: Value) -> Value {
    let mode = value["mode"].as_u64().unwrap_or(0);
    let permissions: String = (0..9)
        .map(|i| {
            if mode & (1 << (8 - i)) != 0 {
                ['r', 'w', 'x'][i % 3]
            } else {
                '-'
            }
        })
        .collect();
    let mut result = json!({"path":value["path"],"name":value["name"],"type":if value["type"] == "dir" {"FILE_TYPE_DIRECTORY"} else {"FILE_TYPE_FILE"},"size":value["size"].as_u64().unwrap_or(0).to_string(),"mode":mode,"permissions":permissions,"owner":value["owner"],"group":value["group"],"modifiedTime":chrono::DateTime::from_timestamp(value["modified"].as_i64().unwrap_or(0),0).unwrap_or_default().to_rfc3339(),"isSymlink":value["isSymlink"]});
    if !value["symlinkTarget"].is_null() {
        result["symlinkTarget"] = value["symlinkTarget"].clone();
    }
    result
}
fn path(body: &Value, key: &str) -> Result<PathBuf, Error> {
    guest_path(
        body[key]
            .as_str()
            .ok_or_else(|| invalid(format!("{key} required")))?,
        true,
    )
}
fn object_keys(body: &Value, allowed: &[&str]) -> Result<(), Error> {
    let object = body
        .as_object()
        .ok_or_else(|| invalid("JSON object required"))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(unsupported("unsupported native RPC option"));
    }
    Ok(())
}
pub(super) async fn handle(service: Service, request: Request) -> Result<Response, Error> {
    let method = request.method().clone();
    let route = request.uri().path().to_owned();
    let user = username(&request)?;
    let response = match (method.as_str(), route.as_str()) {
        ("GET", "/health") => {
            service.info().await?;
            StatusCode::NO_CONTENT.into_response()
        }
        ("GET", "/envs") => {
            service.info().await?;
            axum::Json(&service.env).into_response()
        }
        ("GET", "/files") => {
            if user
                .as_deref()
                .is_some_and(|user| user != "root" && user != "0")
            {
                return Err(unsupported("native file API supports root only"));
            }
            if query(&request, "username").is_some_and(|u| u != "root" && u != "0") {
                return Err(unsupported("native file API supports root only"));
            }
            let path = guest_path(
                &query(&request, "path").ok_or_else(|| invalid("path required"))?,
                true,
            )?;
            service.download(path, None).await?
        }
        ("POST", "/files") => {
            if user
                .as_deref()
                .is_some_and(|user| user != "root" && user != "0")
            {
                return Err(unsupported("native file API supports root only"));
            }
            axum::Json(opensandbox::multipart(&service, request, true).await?).into_response()
        }
        ("POST", route)
            if route.starts_with("/process.Process/")
                || route.starts_with("/filesystem.Filesystem/") =>
        {
            if request
                .headers()
                .get(header::CONTENT_ENCODING)
                .is_some_and(|value| value.as_bytes() != b"identity")
            {
                return Err(unsupported("RPC compression unsupported"));
            }
            let content_type = request
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned();
            let stream = route.ends_with("/Start") || route.ends_with("/Connect");
            if content_type.split(';').next()
                != Some(if stream {
                    "application/connect+json"
                } else {
                    "application/json"
                })
            {
                return Err(unsupported("native RPC requires JSON Connect encoding"));
            }
            let bytes = axum::body::to_bytes(request.into_body(), MAX_FRAME + 5)
                .await
                .map_err(|_| invalid("RPC body exceeds limit"))?;
            let body: Value = if stream {
                decode(&bytes)?
            } else {
                serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?
            };
            if route.starts_with("/filesystem.")
                && user.as_deref().is_some_and(|u| u != "root" && u != "0")
            {
                return Err(unsupported("native file API supports root only"));
            }
            match route {
                "/process.Process/Start" => {
                    object_keys(&body, &["process", "pty", "tag", "stdin"])?;
                    let process = &body["process"];
                    object_keys(process, &["cmd", "args", "envs", "cwd"])?;
                    let mut argv = vec![
                        process["cmd"]
                            .as_str()
                            .ok_or_else(|| invalid("cmd required"))?
                            .to_owned(),
                    ];
                    if let Some(args) = process.get("args") {
                        for arg in args
                            .as_array()
                            .ok_or_else(|| invalid("args must be strings"))?
                        {
                            argv.push(
                                arg.as_str()
                                    .ok_or_else(|| invalid("invalid argument"))?
                                    .to_owned(),
                            );
                        }
                    }
                    let env = process
                        .get("envs")
                        .map(|v| {
                            serde_json::from_value(v.clone()).map_err(|e| invalid(e.to_string()))
                        })
                        .transpose()?
                        .unwrap_or_default();
                    let cwd = process
                        .get("cwd")
                        .and_then(Value::as_str)
                        .map(|p| guest_path(p, true))
                        .transpose()?
                        .or(Some("/root".into()));
                    let tty = if let Some(pty) = body.get("pty") {
                        let rows = pty["size"]["rows"].as_u64().unwrap_or(24);
                        let cols = pty["size"]["cols"].as_u64().unwrap_or(80);
                        Some([
                            u16::try_from(rows).map_err(|_| invalid("PTY rows out of range"))?,
                            u16::try_from(cols).map_err(|_| invalid("PTY columns out of range"))?,
                        ])
                    } else {
                        None
                    };
                    let tag = body.get("tag").and_then(Value::as_str).map(str::to_owned);
                    let job = service
                        .start(
                            ExecLaunch {
                                argv,
                                env,
                                cwd,
                                user,
                                tty,
                            },
                            body["stdin"].as_bool().unwrap_or(true),
                            tag,
                        )
                        .await?;
                    opensandbox::events(job, true, None)
                }
                "/process.Process/Connect" => {
                    object_keys(&body, &["process"])?;
                    opensandbox::events(service.job(&body["process"])?, true, None)
                }
                "/process.Process/List" => {
                    object_keys(&body, &[])?;
                    let jobs = service.jobs.lock().unwrap();
                    axum::Json(json!({"processes":jobs.iter().filter(|job|!job.done()).map(|job|json!({"pid":job.pid,"tag":job.tag,"config":{"cmd":job.launch.argv[0],"args":job.launch.argv[1..],"envs":job.launch.env,"cwd":job.launch.cwd}})).collect::<Vec<_>>()})).into_response()
                }
                "/process.Process/SendInput" => {
                    object_keys(&body, &["process", "input"])?;
                    let job = service.job(&body["process"])?;
                    let value = body["input"]["stdin"]
                        .as_str()
                        .or(body["input"]["pty"].as_str())
                        .ok_or_else(|| invalid("input required"))?;
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(value)
                        .map_err(|_| invalid("input must be base64"))?;
                    job.send_input(&bytes).await?;
                    axum::Json(json!({})).into_response()
                }
                "/process.Process/CloseStdin" => {
                    object_keys(&body, &["process"])?;
                    service.job(&body["process"])?.close_stdin().await?;
                    axum::Json(json!({})).into_response()
                }
                "/process.Process/Update" => {
                    object_keys(&body, &["process", "pty"])?;
                    let job = service.job(&body["process"])?;
                    let rows = body["pty"]["size"]["rows"]
                        .as_u64()
                        .and_then(|v| u16::try_from(v).ok())
                        .filter(|v| *v > 0)
                        .ok_or_else(|| invalid("nonzero PTY rows required"))?;
                    let columns = body["pty"]["size"]["cols"]
                        .as_u64()
                        .and_then(|v| u16::try_from(v).ok())
                        .filter(|v| *v > 0)
                        .ok_or_else(|| invalid("nonzero PTY columns required"))?;
                    ensure_ready(
                        service
                            .rpc(&GuestRequest::Resize {
                                pid: job.pid,
                                generation: job.generation,
                                rows,
                                columns,
                            })
                            .await?,
                    )?;
                    axum::Json(json!({})).into_response()
                }
                "/process.Process/SendSignal" => {
                    object_keys(&body, &["process", "signal"])?;
                    let signal = match body["signal"].as_str() {
                        Some("SIGNAL_SIGTERM") => 15,
                        Some("SIGNAL_SIGKILL") => 9,
                        _ => body["signal"]
                            .as_i64()
                            .filter(|n| *n == 9 || *n == 15)
                            .ok_or_else(|| invalid("unsupported signal"))?
                            as i32,
                    };
                    let job = service.job(&body["process"])?;
                    service.signal(&job, signal).await?;
                    axum::Json(json!({})).into_response()
                }
                "/filesystem.Filesystem/Stat" => {
                    object_keys(&body, &["path"])?;
                    axum::Json(json!({"entry":entry(service.file(FileRequest::Stat { path:path(&body,"path")? }).await?)})).into_response()
                }
                "/filesystem.Filesystem/MakeDir" => {
                    object_keys(&body, &["path"])?;
                    axum::Json(json!({"entry":entry(service.file(FileRequest::MakeDir { path:path(&body,"path")?,mode:0o755 }).await?)})).into_response()
                }
                "/filesystem.Filesystem/Remove" => {
                    object_keys(&body, &["path"])?;
                    service
                        .file(FileRequest::Remove {
                            path: path(&body, "path")?,
                        })
                        .await?;
                    axum::Json(json!({})).into_response()
                }
                "/filesystem.Filesystem/Move" => {
                    object_keys(&body, &["source", "destination"])?;
                    axum::Json(json!({"entry":entry(service.file(FileRequest::Move { source:path(&body,"source")?,destination:path(&body,"destination")? }).await?)})).into_response()
                }
                "/filesystem.Filesystem/ListDir" => {
                    object_keys(&body, &["path", "depth"])?;
                    if body["depth"].as_u64().unwrap_or(1) > 1 {
                        return Err(unsupported("recursive native listing unsupported"));
                    }
                    let list = service
                        .file(FileRequest::List {
                            path: path(&body, "path")?,
                        })
                        .await?;
                    axum::Json(json!({"entries":list.as_array().context("invalid guest list")?.iter().cloned().map(entry).collect::<Vec<_>>()})).into_response()
                }
                _ => {
                    return Err(unsupported(
                        "operation is unsupported by the native E2B core profile",
                    ));
                }
            }
        }
        _ => {
            return Err(unsupported(
                "operation is unsupported by the native E2B core profile",
            ));
        }
    };
    let mut response = response;
    response
        .headers_mut()
        .insert("x-envd-version", "0.10.2".parse().unwrap());
    response
        .headers_mut()
        .insert("x-pvisor-backend", "native-guest".parse().unwrap());
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connect_json_lengths_and_flags_are_strict() {
        let value = json!({"process":{"cmd":"/bin/bash","args":["-c","printf 雪"]}});
        let frame = envelope(0, &value);
        assert_eq!(decode(&frame).unwrap(), value);
        for size in 0..frame.len() {
            assert!(decode(&frame[..size]).is_err());
        }
        assert!(decode(&envelope(1, &value)).is_err());
        let mut trailing = frame;
        trailing.push(0);
        assert!(decode(&trailing).is_err());
    }
}
