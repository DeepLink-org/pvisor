//! E2B protocol routing over the shared Home queue and executor claims.
use super::super::{claims::QueuedCreate, e2b as protocol};
use super::*;

pub(super) fn route(path: &str) -> bool {
    matches!(path, "/sandboxes" | "/v2/sandboxes" | "/e2b")
        || path.starts_with("/sandboxes/")
        || path.starts_with("/v2/sandboxes/")
        || path.starts_with("/e2b/")
}

pub(super) fn control_path(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or_default();
    if matches!(path, "/sandboxes" | "/v2/sandboxes") {
        return true;
    }
    let rest = path
        .strip_prefix("/sandboxes/")
        .or_else(|| path.strip_prefix("/v2/sandboxes/"));
    rest.is_some_and(|rest| {
        let (id, suffix) = rest.split_once('/').unwrap_or((rest, ""));
        valid_local(id) && !suffix.contains(['%', '\\'])
    })
}

fn globalize_e2b(value: &mut Value, owner: &str) {
    if let Some(id) = value["sandboxID"].as_str() {
        value["sandboxID"] = json!(format!("{owner}.{id}"));
    }
}

impl Cluster {
    pub(super) async fn handle_e2b(&self, mut request: Request) -> Result<Response, ApiError> {
        let path = request.uri().path().to_owned();
        let data = path == "/e2b" || path.starts_with("/e2b/");
        let collection = matches!(path.as_str(), "/sandboxes" | "/v2/sandboxes");
        let prefix = if path.starts_with("/v2/") {
            "/v2/sandboxes/"
        } else {
            "/sandboxes/"
        };
        let id = if data {
            protocol::one(request.headers(), "e2b-sandbox-id")
        } else {
            path.strip_prefix(prefix).and_then(|v| v.split('/').next())
        }
        .unwrap_or_default()
        .to_owned();
        // Legacy local IDs and unsupported local routes retain their exact adapter behavior.
        if !collection && !id.contains('.') {
            return Ok(self.local.clone().oneshot(request).await.unwrap());
        }
        let _ingress = self
            .ingress
            .try_acquire()
            .map_err(|_| unavailable("cluster inflight limit"))?;
        if !data
            && !protocol::one(request.headers(), "x-api-key").is_some_and(|key| {
                super::super::secret_eq(key.as_bytes(), self.daemon.config.api_key.as_bytes())
            })
        {
            return Err(ApiError::new(
                401,
                "UNAUTHORIZED",
                "Missing or invalid E2B API key",
            ));
        }
        if collection {
            if request.method() == Method::GET {
                return self.e2b_list(request).await;
            }
            if request.method() != Method::POST {
                return Ok(self.local.clone().oneshot(request).await.unwrap());
            }
            require_json(&request)?;
            let Call::Http { body, .. } = self.control_call(request).await? else {
                unreachable!()
            };
            let create: protocol::Create = serde_json::from_slice(&body)
                .map_err(|_| ApiError::bad_request("Invalid E2B JSON request"))?;
            let create = create
                .validate(self.daemon.config.max_timeout_seconds)
                .map_err(|e| e.0)?;
            let profile = self.e2b_profile(&create.template).await?;
            let body = serde_json::to_vec(&QueuedCreate::E2b {
                e2b_request: create,
                profile,
            })
            .map_err(unavailable)?;
            let response = self.enqueue_wait(body).await?;
            let (mut parts, body) = response.into_parts();
            let value: Value =
                serde_json::from_slice(&to_bytes(body, REPLY).await.map_err(unavailable)?)
                    .map_err(unavailable)?;
            if parts.status != StatusCode::ACCEPTED {
                let mut value = value;
                value["error_code"] = value["code"].clone();
                value["code"] = json!(parts.status.as_u16());
                parts.headers.remove("content-length");
                return Ok(Response::from_parts(
                    parts,
                    Body::from(serde_json::to_vec(&value).map_err(unavailable)?),
                ));
            }
            let id = value["id"]
                .as_str()
                .ok_or_else(|| unavailable("missing assigned ID"))?;
            let (owner, local) = split_id(id)?;
            // Credentials stay on the executor, never in the Home completion ledger.
            // Failure here retains the task header so the accepted creation is recoverable.
            let mut result = match self
                .e2b_control(owner, Method::GET, format!("/sandboxes/{local}"), vec![])
                .await
            {
                Ok(mut response) => {
                    if response.status().is_success() {
                        *response.status_mut() = StatusCode::CREATED;
                    }
                    response
                }
                Err(error) => protocol::Error(error).into_response(),
            };
            if let Some(task) = parts.headers.remove("x-pvisor-task-id") {
                result.headers_mut().insert("x-pvisor-task-id", task);
            }
            return Ok(result);
        }
        let (owner, local) = split_id(&id)?;
        if data {
            // Never synthesize an API key. The executor authenticates the original scoped token.
            request
                .headers_mut()
                .insert("e2b-sandbox-id", local.parse().map_err(unavailable)?);
            if owner == self.owner {
                return Ok(self.local.clone().oneshot(request).await.unwrap());
            }
            let (_, peer) = self.owner_peer(owner).await.map_err(unavailable)?;
            return self.proxy(request, &peer.authority).await;
        }
        let suffix = path.strip_prefix(&format!("{prefix}{id}")).unwrap();
        let target = format!(
            "{prefix}{local}{suffix}{}",
            request
                .uri()
                .query()
                .map(|q| format!("?{q}"))
                .unwrap_or_default()
        );
        if !control_path(&target) {
            return Err(ApiError::bad_request("invalid E2B lifecycle path"));
        }
        // Match the adapter's JSON handlers, not its body-independent unsupported fallback.
        if request.method() == Method::POST
            && (suffix == "/connect"
                || (prefix == "/sandboxes/" && matches!(suffix, "/timeout" | "/resume" | "/pause")))
        {
            require_json(&request)?;
        }
        *request.uri_mut() = target
            .parse()
            .map_err(|_| ApiError::bad_request("invalid URI"))?;
        let Call::Http { method, path, body } = self.control_call(request).await? else {
            unreachable!()
        };
        self.e2b_control(owner, method.parse().map_err(unavailable)?, path, body)
            .await
    }

    async fn e2b_profile(&self, image: &str) -> Result<crate::runtime::E2bTemplate, ApiError> {
        if let Ok(profile) = self.daemon.runtime.e2b_template(image).await {
            return Ok(profile);
        }
        for (actor, _) in self.peers_inner(false).await.map_err(unavailable)? {
            let result: Result<crate::runtime::E2bTemplate, ApiError> = match self
                .rpc(
                    &actor,
                    Call::E2bTemplate {
                        image: image.into(),
                    },
                    STATUS_TIME,
                )
                .await
            {
                Ok(result) => result,
                Err(_) => continue,
            };
            if let Ok(profile) = result {
                return Ok(profile);
            }
        }
        Err(ApiError::new(
            404,
            "TEMPLATE_NOT_FOUND",
            "Prepared E2B template is unavailable on discovered peers",
        ))
    }

    async fn e2b_control(
        &self,
        owner: &str,
        method: Method,
        path: String,
        body: Vec<u8>,
    ) -> Result<Response, ApiError> {
        let response = if owner == self.owner {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header("x-api-key", &self.daemon.config.api_key)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .map_err(unavailable)?;
            self.local.clone().oneshot(request).await.unwrap()
        } else {
            let (actor, _) = self.owner_peer(owner).await.map_err(unavailable)?;
            let deadline = if method == Method::GET {
                STATUS_TIME
            } else {
                RPC_TIME
            };
            let reply: HttpReply = self
                .rpc(
                    &actor,
                    Call::Http {
                        method: method.to_string(),
                        path,
                        body,
                    },
                    deadline,
                )
                .await
                .map_err(unavailable)?;
            reply.response()?
        };
        let (mut parts, body) = response.into_parts();
        let bytes = to_bytes(body, REPLY).await.map_err(unavailable)?;
        let body = if parts.status.is_success() && !bytes.is_empty() {
            let mut value: Value = serde_json::from_slice(&bytes).map_err(unavailable)?;
            globalize_e2b(&mut value, owner);
            serde_json::to_vec(&value).map_err(unavailable)?
        } else {
            bytes.to_vec()
        };
        parts.headers.remove("content-length");
        Ok(Response::from_parts(parts, Body::from(body)))
    }

    async fn e2b_list(&self, request: Request) -> Result<Response, ApiError> {
        let raw = request.uri().query().unwrap_or_default().to_owned();
        // Local adapter is the sole query grammar authority, before discovery.
        let checked = self.local.clone().oneshot(request).await.unwrap();
        if !checked.status().is_success() {
            return Ok(checked);
        }
        let mut offset = 0usize;
        let mut limit = 100usize;
        let mut descending = true;
        let path = {
            let mut query = url::form_urlencoded::Serializer::new(String::new());
            for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
                match key.as_ref() {
                    "nextToken" => offset = value.parse().expect("validated pagination"),
                    "limit" => limit = value.parse().expect("validated pagination"),
                    "order" => {
                        descending = value == "desc";
                        query.append_pair(&key, &value);
                    }
                    _ => {
                        query.append_pair(&key, &value);
                    }
                }
            }
            query.append_pair("limit", &INVENTORY_PAGE.to_string());
            format!("/sandboxes?{}", query.finish())
        };
        let peers = self.peers_inner(true).await.map_err(unavailable)?;
        let mut items = Vec::new();
        let mut bytes = 0usize;
        let mut ids = BTreeSet::new();
        for (actor, peer) in peers {
            let mut consumed = 0usize;
            loop {
                let reply: HttpReply = self
                    .rpc(
                        &actor,
                        Call::Http {
                            method: "GET".into(),
                            path: format!("{path}&nextToken={consumed}"),
                            body: vec![],
                        },
                        STATUS_TIME,
                    )
                    .await
                    .map_err(unavailable)?;
                if reply.status != 200 {
                    return Err(unavailable("peer E2B inventory unavailable"));
                }
                charge_inventory(&mut bytes, reply.body.len())?;
                let page: Vec<Value> = serde_json::from_slice(&reply.body).map_err(unavailable)?;
                if page.len() > INVENTORY_PAGE {
                    return Err(unavailable("invalid inventory page"));
                }
                let count = page.len();
                for mut item in page {
                    let id = item["sandboxID"]
                        .as_str()
                        .filter(|id| valid_local(id))
                        .ok_or_else(|| unavailable("invalid inventory ID"))?;
                    let global_id = format!("{}.{id}", peer.owner);
                    if !ids.insert(global_id.clone()) || items.len() >= INVENTORY_ITEMS {
                        return Err(unavailable("inventory bound/duplicate"));
                    }
                    charge_inventory(&mut bytes, peer.owner.len() + 1)?;
                    globalize_e2b(&mut item, &peer.owner);
                    let started: chrono::DateTime<chrono::Utc> =
                        serde_json::from_value(item["startedAt"].clone()).map_err(unavailable)?;
                    items.push((started, global_id, item));
                }
                if count < INVENTORY_PAGE {
                    break;
                }
                consumed += count;
            }
        }
        items.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        if descending {
            items.reverse();
        }
        let total = items.len();
        let body = serde_json::to_vec(
            &items
                .into_iter()
                .skip(offset)
                .take(limit)
                .map(|(_, _, item)| item)
                .collect::<Vec<_>>(),
        )
        .map_err(unavailable)?;
        if body.len() > REPLY {
            return Err(unavailable("inventory response budget"));
        }
        let mut response = ([("content-type", "application/json")], body).into_response();
        if let Some(next) = offset.checked_add(limit).filter(|next| *next < total) {
            response
                .headers_mut()
                .insert("x-next-token", next.to_string().parse().unwrap());
        }
        Ok(response)
    }
}

fn require_json(request: &Request) -> Result<(), ApiError> {
    let media = request
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if media == "application/json"
        || (media.starts_with("application/") && media.ends_with("+json"))
    {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "Expected request with Content-Type: application/json",
        ))
    }
}
