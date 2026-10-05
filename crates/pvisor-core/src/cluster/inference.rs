//! A bounded, lease-fenced cooperative inference wait. Parallel calls share a
//! Worker-owned wait group; any ready member wakes the whole guest.
use super::*;

pub const INFERENCE_CONTROL_PREFIX: &str = "pvisor-inference-";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceWaitKey {
    pub lease: LeaseKey,
    /// Monotonic within this Attempt; never reused for another wait group.
    pub revision: u64,
    /// First authorized Gateway call in the group; no model payload or secrets.
    pub call_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceWaitIntent {
    Begin,
    Ready,
    Observe,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceWaitRequest {
    pub key: InferenceWaitKey,
    pub intent: InferenceWaitIntent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceWaitRecord {
    pub key: InferenceWaitKey,
    pub pause_revision: u64,
    pub resume_revision: Option<u64>,
    pub ready: bool,
    /// A human control or native termination revoked automatic pause ownership.
    pub interrupted: bool,
    pub requested_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceWaitReceipt {
    pub record: InferenceWaitRecord,
    /// Pause confirmed, or the wait was already resumed without another pause.
    pub entered: bool,
    /// Current lease, full admission, running guest, no pending VM transition.
    pub delivery_ready: bool,
}
