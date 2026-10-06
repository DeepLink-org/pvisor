use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
}
impl ApiError {
    pub fn new(status: u16, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
        }
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, "INVALID_REQUEST", message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(500, "INTERNAL_ERROR", message)
    }
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for ApiError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Image {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateRequest {
    pub image: Option<Image>,
    pub snapshot_id: Option<String>,
    pub template_id: Option<String>,
    pub entrypoint: Option<Vec<String>>,
    pub resource_limits: Option<BTreeMap<String, String>>,
    pub timeout: Option<u64>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    pub extensions: Option<BTreeMap<String, String>>,
    pub platform: Option<serde_json::Value>,
    pub resource_requests: Option<serde_json::Value>,
    pub network_policy: Option<serde_json::Value>,
    pub credential_proxy: Option<serde_json::Value>,
    pub secure_access: Option<serde_json::Value>,
    pub volumes: Option<serde_json::Value>,
    pub lifecycle: Option<serde_json::Value>,
}

pub(super) struct Validated {
    pub image: Image,
    pub entrypoint: Vec<String>,
    pub timeout: Option<u64>,
    pub env: BTreeMap<String, String>,
    pub metadata: BTreeMap<String, String>,
    pub cpu_millis: u64,
    pub memory_bytes: u64,
}

impl CreateRequest {
    pub(super) fn validate(self, maximum_timeout: u64) -> Result<Validated, ApiError> {
        if self.snapshot_id.is_some()
            || self.template_id.is_some()
            || self.network_policy.is_some()
            || self.credential_proxy.is_some()
            || self.secure_access.is_some()
            || self.volumes.is_some()
            || self.lifecycle.is_some()
            || self.resource_requests.is_some()
            || self
                .extensions
                .as_ref()
                .is_some_and(|extensions| !extensions.is_empty())
        {
            return Err(ApiError::new(
                501,
                "NOT_SUPPORTED",
                "snapshots, templates, network policies, credential proxies, secure access, volumes, lifecycle hooks, resource requests and extensions are not supported",
            ));
        }
        let image = self
            .image
            .ok_or_else(|| ApiError::bad_request("image is required"))?;
        if image.auth.is_some() {
            return Err(ApiError::new(
                501,
                "NOT_SUPPORTED",
                "image auth is not supported; provision images on the node",
            ));
        }
        if image.uri.is_empty()
            || image.uri.starts_with('-')
            || image.uri.contains('\0')
            || image.uri.len() > 2048
        {
            return Err(ApiError::bad_request("invalid image URI"));
        }
        let entrypoint = self
            .entrypoint
            .ok_or_else(|| ApiError::bad_request("image creation requires entrypoint"))?;
        if entrypoint.is_empty()
            || entrypoint.len() > 256
            || entrypoint
                .iter()
                .any(|arg| arg.contains('\0') || arg.len() > 65536)
        {
            return Err(ApiError::bad_request("invalid entrypoint argv"));
        }
        if entrypoint[0].is_empty() {
            return Err(ApiError::bad_request(
                "entrypoint executable must not be empty",
            ));
        }
        if self.timeout.is_some_and(|timeout| {
            timeout < 60 || timeout > maximum_timeout || timeout > i64::MAX as u64
        }) {
            return Err(ApiError::bad_request(
                "timeout must be at least 60 seconds and within the node limit",
            ));
        }
        if let Some(platform) = self.platform {
            let arch = match std::env::consts::ARCH {
                "x86_64" => "amd64",
                "aarch64" => "arm64",
                other => other,
            };
            let Some(object) = platform.as_object() else {
                return Err(ApiError::bad_request("platform must be an object"));
            };
            if object.len() != 2
                || object.get("os").and_then(|v| v.as_str()) != Some("linux")
                || object.get("arch").and_then(|v| v.as_str()) != Some(arch)
            {
                return Err(ApiError::new(
                    501,
                    "NOT_SUPPORTED",
                    "only the native Linux platform is supported",
                ));
            }
        }
        validate_map(&self.env, true)?;
        validate_map(&self.metadata, false)?;
        let limits = self
            .resource_limits
            .ok_or_else(|| ApiError::bad_request("resourceLimits is required"))?;
        if limits.len() != 2 || !limits.contains_key("cpu") || !limits.contains_key("memory") {
            return Err(ApiError::bad_request(
                "resourceLimits must contain exactly cpu and memory",
            ));
        }
        let cpu_millis = cpu_quantity(&limits["cpu"])?;
        let memory_bytes = memory_quantity(&limits["memory"])?;
        Ok(Validated {
            image,
            entrypoint,
            timeout: self.timeout,
            env: self.env,
            metadata: self.metadata,
            cpu_millis,
            memory_bytes,
        })
    }
}

fn validate_map(values: &BTreeMap<String, String>, environment: bool) -> Result<(), ApiError> {
    if values.len() > 256
        || values.iter().any(|(key, value)| {
            key.is_empty()
                || key.len() > 256
                || value.len() > 65536
                || key.contains('\0')
                || value.contains('\0')
                || (environment && key.contains('='))
        })
    {
        return Err(ApiError::bad_request(
            "invalid or oversized environment/metadata",
        ));
    }
    Ok(())
}

fn decimal(value: &str, multiplier: u64) -> Result<u64, ApiError> {
    let bad = || ApiError::bad_request("invalid, unrepresentable or overflowing resource quantity");
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 9
    {
        return Err(bad());
    }
    let whole: u64 = whole.parse().map_err(|_| bad())?;
    let base = whole.checked_mul(multiplier).ok_or_else(bad)?;
    let extra = if fraction.is_empty() {
        0
    } else {
        let scale = 10u64.pow(fraction.len() as u32);
        let numerator = fraction
            .parse::<u64>()
            .map_err(|_| bad())?
            .checked_mul(multiplier)
            .ok_or_else(bad)?;
        if numerator % scale != 0 {
            return Err(bad());
        }
        numerator / scale
    };
    let result = base.checked_add(extra).ok_or_else(bad)?;
    if result == 0 {
        return Err(bad());
    }
    Ok(result)
}

fn cpu_quantity(value: &str) -> Result<u64, ApiError> {
    match value.strip_suffix('m') {
        Some(millis) => decimal(millis, 1),
        None => decimal(value, 1000),
    }
}
fn memory_quantity(value: &str) -> Result<u64, ApiError> {
    for (suffix, multiplier) in [
        ("Ki", 1u64 << 10),
        ("Mi", 1 << 20),
        ("Gi", 1 << 30),
        ("Ti", 1 << 40),
        ("K", 1000),
        ("M", 1_000_000),
        ("G", 1_000_000_000),
        ("T", 1_000_000_000_000),
    ] {
        if let Some(number) = value.strip_suffix(suffix) {
            return decimal(number, multiplier);
        }
    }
    decimal(value, 1)
}

#[derive(Debug, Clone, Serialize)]
pub struct EndpointResponse {
    pub endpoint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenewRequest {
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxStatus {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_transition_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sandbox {
    pub id: String,
    pub status: SandboxStatus,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    pub image: Image,
    pub entrypoint: Vec<String>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub sandbox: Sandbox,
    pub env: BTreeMap<String, String>,
    pub cpu_millis: u64,
    pub memory_bytes: u64,
    pub endpoint_token: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Registry {
    pub version: u32,
    pub owner: String,
    pub sandboxes: BTreeMap<String, Record>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_response_preserves_wire_shape() {
        let mut response = EndpointResponse {
            endpoint: "localhost:8080/v1/sandboxes/sb-fixture/proxy/44772".into(),
            headers: None,
        };
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            serde_json::json!({"endpoint": response.endpoint})
        );
        response.headers = Some(BTreeMap::from([(
            "X-PVISOR-SANDBOX-TOKEN".into(),
            "sandbox-token".into(),
        )]));
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            serde_json::json!({
                "endpoint": response.endpoint,
                "headers": {"X-PVISOR-SANDBOX-TOKEN": "sandbox-token"}
            })
        );
    }
    #[test]
    fn quantities_are_exact_and_checked() {
        assert_eq!(cpu_quantity("500m").unwrap(), 500);
        assert_eq!(cpu_quantity("1.25").unwrap(), 1250);
        assert_eq!(memory_quantity("1.5Gi").unwrap(), 1610612736);
        for value in ["0", "-1", "NaN", "inf", "0.0001", "18446744073709551615"] {
            assert!(cpu_quantity(value).is_err(), "{value}");
        }
    }
    #[test]
    fn image_requires_argv_and_hard_limits() {
        let request: CreateRequest =
            serde_json::from_value(serde_json::json!({"image":{"uri":"fixture"}})).unwrap();
        assert_eq!(request.validate(3600).err().unwrap().status, 400);
    }
}
