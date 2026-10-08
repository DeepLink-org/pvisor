//! Typed in-process registry helpers.

use anyhow::Result;
use std::sync::Mutex;

use super::super::actors::RunActor;
use serde_json::Value;

use crate::record::CaptureRecord;
use crate::session::storage::CaptureRoute;
use crate::subagent_link::SpawnLinkBackfill;

use super::super::CallContext;

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
    fn typed_main_route_resolves_unregistered_subagent_to_root() {
        let route = CaptureRoute {
            root_session: Some("run".into()),
            session_id: "s".into(),
            storage_session_id: "s".into(),
            subagent_id: Some("child".into()),
        };
        let run = Mutex::new(RunActor::new());
        let main = run_main_route(&run, &route);
        assert_eq!(main.root_session, route.root_session);
        assert_eq!(main.session_id, route.session_id);
        assert_eq!(main.storage_session_id, "run");
        assert_eq!(main.subagent_id, None);
    }
}
