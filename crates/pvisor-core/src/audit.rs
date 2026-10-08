//! Permission prompt protocol. The owning runtime supplies transport and caching.
use crate::NetworkTransport;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditKind {
    File,
    Network,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditScope {
    pub run_id: String,
    pub attempt_id: String,
    pub view: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<AuditScope>,
    pub kind: AuditKind,
    pub target: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<NetworkTransport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditDecision {
    Allow,
    Deny,
}

pub trait AuditChannel: Send + Sync {
    fn enabled(&self) -> bool;
    fn socket(&self) -> Option<PathBuf>;
    fn request_at(&self, socket: &Path, session: &str, prompt: &AuditRequest) -> AuditDecision;
}
static CHANNEL: OnceLock<Arc<dyn AuditChannel>> = OnceLock::new();
pub fn install(channel: Arc<dyn AuditChannel>) {
    let _ = CHANNEL.set(channel);
}
pub fn enabled() -> bool {
    CHANNEL.get().is_some_and(|channel| channel.enabled())
}
pub fn configured() -> bool {
    CHANNEL.get().is_some()
}
pub fn socket() -> Option<PathBuf> {
    CHANNEL.get().and_then(|channel| channel.socket())
}
pub fn request_at(socket: &Path, session: &str, prompt: &AuditRequest) -> AuditDecision {
    CHANNEL.get().map_or(AuditDecision::Deny, |channel| {
        channel.request_at(socket, session, prompt)
    })
}
pub fn request(prompt: &AuditRequest) -> AuditDecision {
    if !enabled() {
        return AuditDecision::Deny;
    }
    socket().map_or(AuditDecision::Deny, |socket| {
        request_at(&socket, "standalone", prompt)
    })
}
