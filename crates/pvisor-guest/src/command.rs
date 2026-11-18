//! Command construction belongs to the guest supervisor, not host consumers.
use pvisor_guest::api::{GuestConfig, GuestValidation};
use std::{io, process::Command};

pub(crate) fn workload(config: &GuestConfig) -> io::Result<Command> {
    config.validate()?;
    let mut command = Command::new(&config.argv[0]);
    command
        .args(&config.argv[1..])
        .env_clear()
        .envs(&config.env)
        .current_dir(&config.cwd);
    Ok(command)
}

pub(crate) fn sidecar(config: &GuestConfig, index: usize) -> io::Result<Command> {
    config.validate()?;
    let sidecar = config.sidecars.get(index).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "guest sidecar index out of range",
        )
    })?;
    let mut command = Command::new(&sidecar.argv[0]);
    command
        .args(&sidecar.argv[1..])
        .env_clear()
        .envs(&config.env)
        .envs(&sidecar.env);
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pvisor_guest::api::SidecarProcess;
    use std::{collections::BTreeMap, path::Path};
    #[rstest::rstest]
    #[case::empty("")]
    #[case::ordinary("44772")]
    #[case::literal("space ' quote \"\n$HOME; value")]
    fn sidecars_validate_optionally_and_overlay_workload_environment(#[case] value: &str) {
        let mut config: GuestConfig =
            serde_json::from_str(r#"{"argv":["/bin/true"],"env":{},"cwd":"/"}"#).unwrap();
        assert!(
            !serde_json::to_value(&config)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("sidecars")
        );
        let definition = SidecarProcess {
            argv: vec!["/.payload".into(), "--serve".into()],
            env: BTreeMap::from([("PAYLOAD_PORT".into(), value.into())]),
            log: "/.payload.log".into(),
        };
        config.sidecars = vec![definition.clone()];
        assert!(config.validate().is_ok());
        let encoded = serde_json::to_vec(&config).unwrap();
        let decoded: GuestConfig = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.sidecars, vec![definition]);
        let mut bad = config.clone();
        bad.sidecars[0].argv.clear();
        assert!(bad.validate().is_err());
        let mut bad = config.clone();
        bad.sidecars[0].log = "relative.log".into();
        assert!(bad.validate().is_err());
        let mut bad = config.clone();
        bad.sidecars[0].argv.push("bad\0word".into());
        assert!(bad.validate().is_err());
        let mut bad = config.clone();
        bad.sidecars[0].env.insert("A=B".into(), "1".into());
        assert!(bad.validate().is_err());
        let mut foreign: serde_json::Value = serde_json::to_value(&config).unwrap();
        foreign["sidecars"][0]["bogus"] = serde_json::json!(1);
        assert!(serde_json::from_value::<GuestConfig>(foreign).is_err());

        config.env.insert("SHARED".into(), "workload".into());
        config.env.insert("PAYLOAD_PORT".into(), "1".into());
        let command = sidecar(&config, 0).unwrap();
        assert_eq!(
            sidecar(&config, 1).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let mut bad = config.clone();
        bad.argv.clear();
        assert_eq!(
            sidecar(&bad, 0).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let envs: std::collections::BTreeMap<_, _> = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().to_string(),
                    value.map(|v| v.to_string_lossy().to_string()),
                )
            })
            .collect();
        assert_eq!(envs["SHARED"].as_deref(), Some("workload"));
        assert_eq!(envs["PAYLOAD_PORT"].as_deref(), Some(value));
    }
    #[rstest::rstest]
    #[case::empty("")]
    #[case::unicode("你好 🦀")]
    #[case::literal("space ' quote \"\nnewline $HOME; $(false)")]
    fn launch_contract_preserves_values_and_rejects_invalid_input(#[case] value: &str) {
        let config = GuestConfig {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "test -z \"${HOME+x}\" || exit 41; printf '%s' \"$COMPLEX\"; exit 7".into(),
            ],
            env: BTreeMap::from([("COMPLEX".into(), value.into())]),
            cwd: "/".into(),
            ..Default::default()
        };
        let bytes = serde_json::to_vec(&config).unwrap();
        let mut decoded: GuestConfig = serde_json::from_slice(&bytes).unwrap();
        let command = workload(&decoded).unwrap();
        assert_eq!(command.get_current_dir(), Some(Path::new("/")));
        assert_eq!(command.get_program(), "/bin/sh");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            config.argv[1..]
                .iter()
                .map(std::ffi::OsStr::new)
                .collect::<Vec<_>>()
        );
        // The wire contract is validated on every host; executing /bin/sh is Unix-only.
        #[cfg(unix)]
        {
            let mut command = command;
            let output = command.output().unwrap();
            assert_eq!(output.status.code(), Some(7));
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                config.env["COMPLEX"]
            );
        }
        decoded.argv.push("bad\0argument".into());
        assert!(workload(&decoded).is_err());
        decoded.argv.clear();
        assert!(workload(&decoded).is_err());
    }
}
