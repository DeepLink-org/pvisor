//! Installed companion dispatch must preserve the managed Run contract.

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn installed_replay_dispatches_spawn_preserves_safe_and_returns_the_outer_status() {
    let root = tempfile::tempdir().unwrap();
    let replay = root.path().join("pvisor-replay");
    std::fs::copy(env!("CARGO_BIN_EXE_pvisor-replay"), &replay).unwrap();
    let core = root.path().join("pvisor");
    std::fs::write(
        &core,
        r#"#!/bin/sh
printf '%s\n' "$@" > "$0.args"
test "$1" = spawn || exit 64
test "$2" = --config || exit 65
cp "$3" "$0.config"
exit 37
"#,
    )
    .unwrap();
    std::fs::set_permissions(&core, std::fs::Permissions::from_mode(0o700)).unwrap();
    for safe in [false, true] {
        let config = root.path().join("replay.toml");
        std::fs::write(
            &config,
            format!(
                r#"
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 0
agent_entrypoint = "/usr/bin/false"
[run]
timeout_ms = 1234
safe = {safe}
"#
            ),
        )
        .unwrap();
        let status = Command::new(&replay)
            .args(["--config"])
            .arg(config)
            .current_dir(root.path())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(37));
        let args = std::fs::read_to_string(core.with_extension("args")).unwrap();
        assert_eq!(args.lines().next(), Some("spawn"));
        assert_eq!(args.lines().any(|arg| arg == "--safe"), safe);
        let resolved: pvisor::RunConfig =
            toml::from_str(&std::fs::read_to_string(core.with_extension("config")).unwrap())
                .unwrap();
        assert_eq!(resolved.run.profile == pvisor_core::RunProfile::Safe, safe);
        assert_eq!(resolved.run.timeout_ms, Some(1234));
        assert!(resolved.run.command.iter().any(|arg| arg == "replay"));
    }
}
