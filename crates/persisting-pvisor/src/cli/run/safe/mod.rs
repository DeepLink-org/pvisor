//! Safe presets are pVisor CLI patches, parsed and applied before explicit CLI options.

mod files;
use super::{RunConfig, RunExecutorKind};

pub(super) fn patch(requested: &RunConfig, audit: bool) -> Vec<String> {
    let mut args = deny_egress();
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
