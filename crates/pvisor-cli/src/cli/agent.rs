//! Client-specific preset admission and projection; CLI/config syntax stays unchanged.
use anyhow::Context;
use pvisor::{GatewayMode, GatewayProfile, OverlayNetMode, RunConfig, RunExecutorKind};
use pvisor_core::RunSpec;
use std::path::Path;

pub(super) fn apply_profile(
    config: &mut RunConfig,
    profile: Option<GatewayProfile>,
    gateway_mode: Option<GatewayMode>,
    network_mode: Option<OverlayNetMode>,
) -> anyhow::Result<()> {
    if let Some(profile) = profile {
        config.gateway.profile = Some(profile);
        anyhow::ensure!(
            gateway_mode != Some(GatewayMode::Off),
            "--gateway-profile conflicts with --gateway-mode off"
        );
        config.gateway.mode = GatewayMode::Capture;
        if network_mode.is_none() {
            config.overlaynet.mode = if config.run.executor == RunExecutorKind::Vm {
                OverlayNetMode::Auto
            } else {
                OverlayNetMode::Proxy
            };
        }
    }
    Ok(())
}

pub(super) fn validate(config: &RunConfig) -> anyhow::Result<()> {
    if let Some(profile) = config.gateway.profile {
        anyhow::ensure!(
            config.gateway.mode == GatewayMode::Capture,
            "Gateway profile requires capture mode"
        );
        anyhow::ensure!(
            config.gateway.routes.is_empty(),
            "Gateway profile cannot be combined with custom model routes"
        );
        anyhow::ensure!(
            config
                .run
                .command
                .first()
                .and_then(|program| Path::new(program).file_name())
                .and_then(|s| s.to_str())
                == Some("zcode"),
            "Gateway profile requires a direct zcode command"
        );
        if profile == GatewayProfile::ZcodeBigmodel {
            let path = config
                .gateway
                .zcode_builtin_config
                .as_deref()
                .context("zcode-bigmodel requires gateway.zcode_builtin_config")?;
            anyhow::ensure!(
                path.is_absolute() && path.is_file(),
                "gateway.zcode_builtin_config must name an existing absolute file"
            );
            anyhow::ensure!(
                config.run.executor == RunExecutorKind::Host,
                "zcode-bigmodel currently requires the host executor"
            );
        }
    } else {
        anyhow::ensure!(
            config.gateway.zcode_builtin_config.is_none(),
            "gateway.zcode_builtin_config requires the zcode-bigmodel profile"
        );
    }
    Ok(())
}

pub(super) fn project(config: &RunConfig, spec: &mut RunSpec) -> anyhow::Result<()> {
    if let Some(path) = &config.gateway.zcode_builtin_config {
        spec.metadata.insert(
            "pvisor.gateway.zcode_builtin_config".into(),
            serde_json::to_value(path.canonicalize()?)?,
        );
    }
    if let Some(profile) = config.gateway.profile {
        spec.metadata.insert(
            "pvisor.gateway.profile".into(),
            serde_json::to_value(profile)?,
        );
    }
    Ok(())
}

#[cfg(any(feature = "gateway", test))]
pub(super) fn routes(config: &RunConfig) -> Vec<pvisor_core::gateway::ModelRoute> {
    config
        .gateway
        .profile
        .map(GatewayProfile::routes)
        .unwrap_or_else(|| config.gateway.routes.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn profile_overrides_keep_capture_and_explicit_network_precedence() {
        let mut config = RunConfig::default();
        apply_profile(&mut config, None, None, None).unwrap();
        assert_eq!(config.gateway.mode, GatewayMode::Off);
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Auto);
        apply_profile(&mut config, Some(GatewayProfile::ZcodeBigmodel), None, None).unwrap();
        assert_eq!(config.gateway.mode, GatewayMode::Capture);
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Proxy);
        config.overlaynet.mode = OverlayNetMode::Off;
        apply_profile(
            &mut config,
            Some(GatewayProfile::ZcodeBigmodel),
            None,
            Some(OverlayNetMode::Off),
        )
        .unwrap();
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Off);
        assert!(
            apply_profile(
                &mut config,
                Some(GatewayProfile::ZcodeBigmodel),
                Some(GatewayMode::Off),
                None
            )
            .unwrap_err()
            .to_string()
            .contains("--gateway-profile conflicts with --gateway-mode off")
        );
    }

    #[test]
    fn gateway_profile_config_and_launch_projection_keep_their_contract() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = directory.path().join("builtin.json");
        std::fs::write(&catalog, br#"{"schemaVersion":1}"#).unwrap();
        let source = format!(
            "[run]\ncommand = ['zcode']\n[gateway]\nmode = 'capture'\n\
             profile = 'zcode-bigmodel'\nzcode_builtin_config = {:?}\n",
            catalog.display().to_string()
        );
        let config: RunConfig = toml::from_str(&source).unwrap();
        validate(&config).unwrap();
        let wire = serde_json::to_value(&config).unwrap();
        assert_eq!(wire["gateway"]["profile"], "zcode-bigmodel");
        assert_eq!(
            wire["gateway"]["zcode_builtin_config"],
            catalog.to_str().unwrap()
        );
        let roundtrip: RunConfig = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(roundtrip).unwrap(), wire);
        assert_eq!(
            serde_json::to_value(routes(&config)).unwrap(),
            json!([{
                "name": "*", "provider": "anthropic",
                "upstream": "https://open.bigmodel.cn/api/anthropic/v1",
                "upstream_anthropic": "https://open.bigmodel.cn/api/anthropic/v1",
                "api_key_env": null, "api_key": null, "forward": null
            }])
        );
        let mut spec = RunSpec::process("job", "zcode", "zcode");
        project(&config, &mut spec).unwrap();
        assert_eq!(spec.metadata["pvisor.gateway.profile"], "zcode-bigmodel");
        assert_eq!(
            spec.metadata["pvisor.gateway.zcode_builtin_config"],
            serde_json::to_value(catalog.canonicalize().unwrap()).unwrap()
        );
        assert_eq!(std::fs::read(&catalog).unwrap(), br#"{"schemaVersion":1}"#);
    }

    #[test]
    fn profile_admission_keeps_host_command_capture_and_route_restrictions() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = directory.path().join("builtin.json");
        std::fs::write(&catalog, "{}").unwrap();
        let mut valid = RunConfig::default();
        valid.run.command = vec!["/opt/bin/zcode".into()];
        valid.gateway.mode = GatewayMode::Capture;
        valid.gateway.profile = Some(GatewayProfile::ZcodeBigmodel);
        valid.gateway.zcode_builtin_config = Some(catalog);
        validate(&valid).unwrap();
        for executor in [RunExecutorKind::Container, RunExecutorKind::Vm] {
            let mut config = valid.clone();
            config.run.executor = executor;
            assert!(
                validate(&config)
                    .unwrap_err()
                    .to_string()
                    .contains("host executor")
            );
        }
        let mut config = valid.clone();
        config.run.command = vec!["sh".into(), "-c".into(), "zcode".into()];
        assert!(
            validate(&config)
                .unwrap_err()
                .to_string()
                .contains("direct zcode")
        );
        let mut config = valid.clone();
        config.gateway.mode = GatewayMode::Off;
        assert!(
            validate(&config)
                .unwrap_err()
                .to_string()
                .contains("capture mode")
        );
        let mut config = valid.clone();
        config.gateway.routes = routes(&valid);
        assert!(
            validate(&config)
                .unwrap_err()
                .to_string()
                .contains("custom model routes")
        );
        let mut config = valid.clone();
        config.gateway.zcode_builtin_config = None;
        assert!(
            validate(&config)
                .unwrap_err()
                .to_string()
                .contains("requires gateway.zcode_builtin_config")
        );
        let mut config = valid.clone();
        config.gateway.zcode_builtin_config = Some("relative.json".into());
        assert!(
            validate(&config)
                .unwrap_err()
                .to_string()
                .contains("existing absolute file")
        );
        let mut config = valid;
        config.gateway.profile = None;
        assert!(
            validate(&config)
                .unwrap_err()
                .to_string()
                .contains("requires the zcode-bigmodel profile")
        );
    }
}
