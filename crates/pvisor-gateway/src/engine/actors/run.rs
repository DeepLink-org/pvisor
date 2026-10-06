//! Run-scoped subagent registry and cross-story links (one per capture runtime).

use crate::record::CaptureRecord;
use crate::subagent_link::{SubagentRegistry, enrich_record, main_route_for_backfill};

use super::super::wire::headers_to_header_map;

/// Run-level subagent spawn links for one proxy instance.
pub(crate) struct RunActor {
    registry: SubagentRegistry,
}

impl RunActor {
    pub fn new() -> Self {
        Self {
            registry: SubagentRegistry::default(),
        }
    }

    pub(crate) fn enrich(
        &mut self,
        record: &mut CaptureRecord,
        ctx: &crate::engine::CallContext,
        body: Option<&serde_json::Value>,
        assistant_text: Option<&str>,
    ) -> anyhow::Result<Vec<crate::subagent_link::SpawnLinkBackfill>> {
        let headers = headers_to_header_map(&ctx.request_headers)?;
        Ok(enrich_record(
            record,
            ctx.route(),
            &headers,
            body,
            assistant_text,
            &mut self.registry,
        )
        .spawn_link_backfills)
    }

    pub(crate) fn main_route(
        &self,
        route: &crate::session::storage::CaptureRoute,
    ) -> crate::session::storage::CaptureRoute {
        main_route_for_backfill(route, &self.registry)
    }
}

impl Default for RunActor {
    fn default() -> Self {
        Self::new()
    }
}
