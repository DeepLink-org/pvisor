//! Typed safe presets, applied through the ordinary override rules before explicit CLI options.
mod files;
use std::path::Path;

use super::{
    GatewayMode, NetworkAccessRule, OverlayFsOverrides, OverlayNetMode, OverlayNetOverrides,
    OverlayNetPolicy, OverlayNetTargetArg, RunArgs, RunConfig, RunExecutorKind, RunOverrides,
};

pub(super) fn patch(requested: &RunConfig) -> RunArgs {
    let program = requested
        .run
        .command
        .first()
        .and_then(|program| Path::new(program).file_name())
        .and_then(|name| name.to_str());
    let gateway_routes =
        requested.gateway.mode == GatewayMode::Capture && !requested.gateway.routes.is_empty();
    let hosts: &[&str] = if gateway_routes {
        &[]
    } else {
        match program {
            // Preserve the shell workflow's existing OpenAI grants. Explicit
            // --overlaynet-* overrides still take precedence after the preset.
            Some("codex" | "bash" | "sh" | "zsh" | "fish") => {
                &["api.openai.com", "chatgpt.com", "ab.chatgpt.com"]
            }
            Some("claude") => &["api.anthropic.com"],
            Some("gemini") => &["generativelanguage.googleapis.com"],
            // API-key OpenAI endpoints: https://zcode.z.ai/cn/docs/configuration
            // ZCode 872ad960de7e rewrites Anthropic endpoints to the shared
            // zcode.z.ai gateway; do not grant that gateway or object storage.
            Some("zcode") => &["api.z.ai", "open.bigmodel.cn"],
            _ => &[],
        }
    };
    RunArgs {
        run: RunOverrides {
            clear_pass_env: true,
            ..Default::default()
        },
        overlaynet: OverlayNetOverrides {
            overlaynet: Some(if requested.run.executor == RunExecutorKind::Vm {
                OverlayNetMode::Auto
            } else {
                OverlayNetMode::Proxy
            }),
            // Unlike deny-all, changing only the policy preserves deny rules and limits.
            overlaynet_policy: hosts.is_empty().then_some(OverlayNetPolicy::Deny),
            overlaynet_allow: hosts
                .iter()
                .map(|host| {
                    OverlayNetTargetArg(NetworkAccessRule {
                        host: (*host).into(),
                        ports: vec![443],
                        transports: Vec::new(),
                        allow_private_ips: false,
                    })
                })
                .collect(),
            ..Default::default()
        },
        overlayfs: OverlayFsOverrides {
            access: files::patch(),
            ..Default::default()
        },
        ..Default::default()
    }
}
