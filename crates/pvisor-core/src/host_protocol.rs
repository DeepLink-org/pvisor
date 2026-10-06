//! Host-authority AgentCtl envelopes. These do not extend cooperative guest authority.
//! Transport and authentication remain endpoint responsibilities.
use serde::{Deserialize, Serialize};

pub const AGENTCTL_HOST_VERSION: u32 = 1;
pub const AGENTCTL_HOST_MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Byte bound for correlation and target identities, independent of frame size.
pub const AGENTCTL_HOST_MAX_ID_BYTES: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCtlHostRequest<C> {
    pub version: u32,
    pub request_id: String,
    pub target: Option<AgentCtlTarget>,
    pub command: C,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCtlTarget {
    pub job_id: String,
    pub attempt_id: Option<String>,
    pub generation: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCtlHostResponse<R> {
    pub version: u32,
    pub request_id: String,
    pub result: Result<R, AgentCtlHostError>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCtlHostError {
    pub code: AgentCtlHostErrorCode,
    pub message: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCtlHostErrorCode {
    InvalidRequest,
    Unauthorized,
    VersionMismatch,
    Conflict,
    Unsupported,
    Internal,
    Unavailable,
}
impl AgentCtlHostError {
    pub fn new(code: AgentCtlHostErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for AgentCtlHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for AgentCtlHostError {}

fn validate_identity(value: &str) -> Result<(), AgentCtlHostError> {
    if value.is_empty()
        || value.len() > AGENTCTL_HOST_MAX_ID_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(AgentCtlHostError::new(
            AgentCtlHostErrorCode::InvalidRequest,
            "invalid host identity",
        ));
    }
    Ok(())
}
fn validate_envelope(version: u32, request_id: &str) -> Result<(), AgentCtlHostError> {
    if version != AGENTCTL_HOST_VERSION {
        return Err(AgentCtlHostError::new(
            AgentCtlHostErrorCode::VersionMismatch,
            "unsupported host protocol version",
        ));
    }
    validate_identity(request_id)
}
impl AgentCtlTarget {
    pub fn validate(&self) -> Result<(), AgentCtlHostError> {
        validate_identity(&self.job_id)?;
        for value in [&self.attempt_id, &self.generation].into_iter().flatten() {
            validate_identity(value)?;
        }
        Ok(())
    }
}
impl<C> AgentCtlHostRequest<C> {
    /// Pure envelope validation; does not authorize a command or resolve a target.
    pub fn validate(&self) -> Result<(), AgentCtlHostError> {
        validate_envelope(self.version, &self.request_id)?;
        if let Some(target) = &self.target {
            target.validate()?;
        }
        Ok(())
    }
}
impl<R> AgentCtlHostResponse<R> {
    pub fn validate(&self, request_id: &str) -> Result<(), AgentCtlHostError> {
        validate_envelope(self.version, &self.request_id)?;
        validate_identity(request_id)?;
        if self.request_id != request_id {
            return Err(AgentCtlHostError::new(
                AgentCtlHostErrorCode::Conflict,
                "host response request ID mismatch",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostSupervisorCommand {
    Inspect,
    Pause,
    Resume,
    Terminate,
    Endpoint { port: u16 },
}
/// Private supervisor namespace credentials, never a guest or public host API token.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSupervisorAuth {
    pub owner: String,
    pub token: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSupervisorRequest {
    pub auth: HostSupervisorAuth,
    pub operation: HostSupervisorCommand,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostSupervisorState {
    Running,
    Paused,
    Stopped,
    Missing,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSupervisorResult {
    pub owner: String,
    pub target: AgentCtlTarget,
    pub state: HostSupervisorState,
    pub endpoint: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> AgentCtlHostRequest<()> {
        AgentCtlHostRequest {
            version: 1,
            request_id: "request".into(),
            target: Some(AgentCtlTarget {
                job_id: "job".into(),
                attempt_id: Some("attempt".into()),
                generation: Some("generation".into()),
            }),
            command: (),
        }
    }
    #[test]
    fn envelope_bounds_and_version() {
        let mut r = request();
        assert!(r.validate().is_ok());
        r.version = 2;
        assert_eq!(
            r.validate().unwrap_err().code,
            AgentCtlHostErrorCode::VersionMismatch
        );
        r.version = 1;
        for id in [String::new(), "x".repeat(257), "bad\nidentity".into()] {
            r.request_id = id;
            assert!(r.validate().is_err());
        }
        r.request_id = "x".repeat(256);
        assert!(r.validate().is_ok());
        for field in 0..3 {
            let mut r = request();
            let t = r.target.as_mut().unwrap();
            match field {
                0 => t.job_id = "x".repeat(257),
                1 => t.attempt_id = Some(String::new()),
                _ => t.generation = Some("x".repeat(257)),
            }
            assert!(r.validate().is_err());
        }
    }
    #[test]
    fn response_correlation_including_errors() {
        let mut r = AgentCtlHostResponse::<()> {
            version: 1,
            request_id: "request".into(),
            result: Err(AgentCtlHostError::new(
                AgentCtlHostErrorCode::Unavailable,
                "reconcile",
            )),
        };
        assert!(r.validate("request").is_ok());
        assert!(r.validate("other").is_err());
        r.version = 2;
        assert!(r.validate("request").is_err());
    }
    #[test]
    fn strict_wire_contracts() {
        let mut value = serde_json::to_value(request()).unwrap();
        value["extra"] = true.into();
        assert!(serde_json::from_value::<AgentCtlHostRequest<()>>(value).is_err());
        assert!(serde_json::from_str::<AgentCtlTarget>(r#"{"job_id":"j","extra":true}"#).is_err());
        assert!(
            serde_json::from_str::<AgentCtlHostError>(
                r#"{"code":"internal","message":"m","extra":true}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<AgentCtlHostResponse<()>>(
                r#"{"version":1,"request_id":"r","result":{"Ok":null},"extra":true}"#
            )
            .is_err()
        );
        assert_eq!(
            serde_json::to_value(HostSupervisorCommand::Endpoint { port: 80 }).unwrap(),
            serde_json::json!({"operation":"endpoint","port":80})
        );
    }
}
