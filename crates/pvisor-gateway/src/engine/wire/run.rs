//! External run actor wire shapes and typed in-process registry helpers.

use anyhow::Result;
use std::sync::Mutex;

use super::super::actors::RunActor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::story::{RunId, StoryId};
use crate::record::CaptureRecord;
use crate::session::storage::CaptureRoute;
use crate::subagent_link::SpawnLinkBackfill;

use super::super::CallContext;

/// Global run actor path (one per capture runtime / ActorSystem).
pub(crate) const RUN_ACTOR_NAME: &str = "capture/run";

/// Commands handled by [`super::super::actors::run::RunActor`].
///
/// `record_bytes` / `body_bytes` are JSON-encoded payloads carried as
/// raw bytes — see [`super::story::StoryCommand`] for rationale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum RunCommand {
    Enrich {
        record_bytes: Vec<u8>,
        route: CaptureRoute,
        headers: Vec<(String, String)>,
        body_bytes: Option<Vec<u8>>,
        assistant_text: Option<String>,
        story_id: Option<StoryId>,
        run_id: Option<RunId>,
    },
    MainRoute {
        route: CaptureRoute,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum RunReply {
    Enrich {
        record_bytes: Vec<u8>,
        backfills: Vec<SpawnLinkBackfill>,
    },
    MainRoute(CaptureRoute),
}

pub(crate) fn run_enrich(
    run: &Mutex<RunActor>,
    rec: &mut CaptureRecord,
    ctx: &CallContext,
    body_json: Option<&Value>,
    assistant_text: Option<&str>,
) -> Result<Vec<SpawnLinkBackfill>> {
    run.lock()
        .unwrap()
        .enrich(rec, ctx, body_json, assistant_text)
}

pub(crate) fn run_main_route(run: &Mutex<RunActor>, route: &CaptureRoute) -> CaptureRoute {
    run.lock().unwrap().main_route(route)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_command_bincode_roundtrip() {
        let route = CaptureRoute {
            root_session: Some("run".into()),
            session_id: "s".into(),
            storage_session_id: "s".into(),
            subagent_id: None,
        };
        let cmd = RunCommand::MainRoute {
            route: route.clone(),
        };
        let packed = pulsing_actor::Message::pack(&cmd).expect("pack");
        let back: RunCommand = packed.unpack().expect("unpack");
        assert!(matches!(back, RunCommand::MainRoute { .. }));
    }
}
