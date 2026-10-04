//! The actions for which Gateway may delegate a configured route credential.
//!
//! Protocol detection is permissive for observation, not authorization: an
//! arbitrary matching suffix must not grant a key.

use axum::http::{HeaderMap, Method, Uri};

use crate::protocol::ProtocolKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GatewayAction<'a> {
    ModelsList,
    ModelRequest {
        protocol: ProtocolKind,
        path_model: Option<&'a str>,
    },
}

// Reject alternate spellings instead of relying on the HTTP parser, URL builder,
// upstream proxy and service agreeing about path normalization.
fn unambiguous_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.contains(['%', '\\', ';'])
        && !path.contains("//")
        && !path
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        && !path.split('/').any(|part| matches!(part, "." | ".."))
}

fn safe_model_segment(model: &str) -> bool {
    !model.is_empty()
        && !matches!(model, "." | "..")
        && model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._".contains(&byte))
}

fn model_path_parts(path: &str) -> Option<(&str, &str, &str)> {
    let path = path.trim_end_matches('/');
    let (prefix, rest) = ["/v1beta/models/", "/v1/models/", "/models/"]
        .into_iter()
        .find_map(|prefix| path.strip_prefix(prefix).map(|rest| (prefix, rest)))?;
    let (model, operation) = rest.split_once(':')?;
    safe_model_segment(model).then_some((prefix, model, operation))
}

fn classify_path(path: &str) -> Option<GatewayAction<'_>> {
    if !unambiguous_path(path) {
        return None;
    }
    let path = path.trim_end_matches('/');
    if matches!(path, "/models" | "/v1/models" | "/v1beta/models") {
        return Some(GatewayAction::ModelsList);
    }
    let endpoint = path
        .strip_prefix("/v1/")
        .unwrap_or_else(|| path.trim_start_matches('/'));
    let protocol = match endpoint {
        "chat/completions" => ProtocolKind::ChatCompletions,
        "messages" => ProtocolKind::Messages,
        "responses" => ProtocolKind::Responses,
        "embeddings" => ProtocolKind::Embeddings,
        "messages/count_tokens" | "count-tokens" => ProtocolKind::CountTokens,
        _ => {
            let (_, model, operation) = model_path_parts(path)?;
            let protocol = match operation {
                "generateContent" | "streamGenerateContent" => ProtocolKind::Gemini,
                "countTokens" => ProtocolKind::CountTokens,
                _ => return None,
            };
            return Some(GatewayAction::ModelRequest {
                protocol,
                path_model: Some(model),
            });
        }
    };
    Some(GatewayAction::ModelRequest {
        protocol,
        path_model: None,
    })
}

pub(super) fn authorize_action<'a>(
    method: &Method,
    uri: &'a Uri,
    headers: &HeaderMap,
) -> Result<GatewayAction<'a>, &'static str> {
    // These conventions can change an allowed POST's method/path downstream.
    // They have no supported model API meaning.
    if [
        "x-http-method-override",
        "x-method-override",
        "x-http-method",
        "x-original-url",
        "x-rewrite-url",
    ]
    .iter()
    .any(|name| headers.contains_key(*name))
    {
        return Err("Gateway does not allow method or path override headers");
    }
    if uri.query().is_some_and(|query| {
        url::form_urlencoded::parse(query.as_bytes()).any(|(key, _)| {
            matches!(
                key.trim().to_ascii_lowercase().as_str(),
                "_method"
                    | "method"
                    | "http_method"
                    | "$httpmethod"
                    | "x-http-method-override"
                    | "x-method-override"
                    | "x-http-method"
                    | "path"
                    | "_path"
                    | "url"
                    | "_url"
                    | "x-original-url"
                    | "x-rewrite-url"
            )
        })
    }) {
        return Err("Gateway does not allow method or path override query parameters");
    }
    let action = classify_path(uri.path())
        .ok_or("Gateway only delegates credentials to supported model API endpoints")?;
    let allowed = match action {
        GatewayAction::ModelsList => method == Method::GET,
        GatewayAction::ModelRequest { .. } => method == Method::POST,
    };
    if !allowed {
        return Err("HTTP method is not allowed for this Gateway model API endpoint");
    }
    Ok(action)
}

// Check the bridge result, too. Native Gemini route forwarding must change the
// URI model, which is the identity the provider actually uses.
pub(super) fn authorize_upstream_path(
    path: &str,
    protocol: ProtocolKind,
    upstream_model: &str,
) -> anyhow::Result<String> {
    let Some(GatewayAction::ModelRequest {
        protocol: actual,
        path_model,
    }) = classify_path(path)
    else {
        anyhow::bail!("protocol bridge produced an unsupported Gateway action");
    };
    anyhow::ensure!(
        actual == protocol,
        "protocol bridge changed the Gateway action protocol"
    );
    if path_model.is_some() {
        let model = upstream_model
            .strip_prefix("models/")
            .unwrap_or(upstream_model);
        anyhow::ensure!(
            safe_model_segment(model),
            "invalid upstream model path segment"
        );
        let (prefix, _, operation) = model_path_parts(path).expect("classified native model path");
        let trailing = if path.ends_with('/') { "/" } else { "" };
        Ok(format!("{prefix}{model}:{operation}{trailing}"))
    } else {
        Ok(path.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_paths_remain_model_actions_and_use_the_routed_model() {
        assert!(
            authorize_upstream_path("/v1/files", ProtocolKind::ChatCompletions, "test").is_err()
        );
        assert!(
            authorize_upstream_path("/v1/messages", ProtocolKind::ChatCompletions, "test").is_err()
        );
        assert_eq!(
            authorize_upstream_path(
                "/v1beta/models/client:generateContent",
                ProtocolKind::Gemini,
                "models/gemini-2.5-pro"
            )
            .unwrap(),
            "/v1beta/models/gemini-2.5-pro:generateContent"
        );
        for model in [".", "..", "a/b", "a%2fb", "a\\b", "a:delete", "a b"] {
            assert!(
                authorize_upstream_path(
                    "/v1/models/client:countTokens",
                    ProtocolKind::CountTokens,
                    model
                )
                .is_err(),
                "{model}"
            );
        }
    }
}
