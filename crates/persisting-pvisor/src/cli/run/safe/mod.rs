//! Safe presets are pVisor CLI patches, parsed and applied before explicit CLI options.

mod claude;
mod codex;
mod files;
mod gemini;
mod zcode;

use std::path::Path;

use super::{GatewayMode, RunConfig, RunExecutorKind};

pub(super) fn patch(requested: &RunConfig) -> Vec<String> {
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
            Some("codex") => codex::patch(),
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
    args.extend(files::patch());
    args
}

fn deny_egress() -> Vec<String> {
    // Unlike --overlaynet-deny-all, changing only the policy preserves deny rules and limits.
    vec!["--overlaynet-policy".into(), "deny".into()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_are_cli_arguments_and_never_select_an_executor() {
        for (agent, expected) in [
            ("codex", vec!["--overlaynet-allow", "api.openai.com:443"]),
            (
                "claude",
                vec!["--overlaynet-allow", "api.anthropic.com:443"],
            ),
            (
                "gemini",
                vec![
                    "--overlaynet-allow",
                    "generativelanguage.googleapis.com:443",
                ],
            ),
            (
                "zcode",
                vec![
                    "--overlaynet-allow",
                    "api.z.ai:443",
                    "--overlaynet-allow",
                    "open.bigmodel.cn:443",
                ],
            ),
            ("unknown", vec!["--overlaynet-policy", "deny"]),
        ] {
            let mut requested = RunConfig::default();
            requested.run.command = vec![agent.into()];
            let args = patch(&requested);
            let mut expected = expected;
            expected.extend(["--overlaynet", "proxy", "--clear-pass-env"]);
            let mut expected = expected.into_iter().map(str::to_owned).collect::<Vec<_>>();
            expected.extend(files::patch());
            assert_eq!(args, expected);
        }
    }
}
