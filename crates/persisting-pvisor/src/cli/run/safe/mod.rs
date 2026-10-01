//! Safe presets are pVisor CLI patches, parsed and applied before explicit CLI options.

mod claude;
mod codex;
mod files;
mod gemini;
mod zcode;
use std::path::Path;

use super::{GatewayMode, RunConfig, RunExecutorKind};

pub(super) fn patch(requested: &RunConfig, audit: bool) -> Vec<String> {
    let program = requested
        .run
        .command
        .first()
        .and_then(|program| Path::new(program).file_name())
        .and_then(|name| name.to_str());
    let gateway_routes =
        requested.gateway.mode == GatewayMode::Capture && !requested.gateway.routes.is_empty();
    let mut args = if gateway_routes {
        deny_egress()
    } else {
        match program {
            Some("codex" | "bash" | "sh" | "zsh" | "fish") => codex::patch(),
            Some("claude") => claude::patch(),
            Some("gemini") => gemini::patch(),
            Some("zcode") => zcode::patch(),
            _ => deny_egress(),
        }
    };
    args.extend([
        "--overlaynet".into(),
        if requested.run.executor == RunExecutorKind::Vm {
            "auto"
        } else {
            "proxy"
        }
        .into(),
        "--clear-pass-env".into(),
    ]);
    args.extend(files::patch(audit));
    args
}

fn deny_egress() -> Vec<String> {
    // Unlike --overlaynet-deny-all, changing only the policy preserves deny rules and limits.
    vec!["--overlaynet-policy".into(), "deny".into()]
}
