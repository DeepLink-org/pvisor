//! Attempt-local Gateway drivers. Route credentials remain in Worker-owned
//! environment/configuration, never controller task or registration records.
use super::*;
use pvisor_core::gateway::{CaptureLevel, ModelRoute};

#[cfg(feature = "gateway")]
#[path = "inference.rs"]
pub(super) mod inference;

#[cfg(feature = "gateway")]
struct ModelController {
    run_id: pvisor_core::RunId,
    allowed_models: Vec<String>,
}
#[cfg(feature = "gateway")]
impl pvisor_core::ControlController for ModelController {
    fn authorize(&self, query: pvisor_core::ControlRequest<'_>) -> pvisor_core::ControlTransition {
        use pvisor_core::{ControlRequest, PolicyControlController};
        match query {
            ControlRequest::Model { policy, request } => {
                // A route may rewrite an alias to another model. Its upstream
                // ID must not grant access to an unauthorized client alias.
                if request.run_id.as_ref() != Some(&self.run_id)
                    || !self.allowed_models.iter().any(|pattern| {
                        pvisor_core::gateway::model_matches(pattern, &request.client_model)
                    })
                {
                    return pvisor_core::ControlTransition::denied(
                        pvisor_core::ControlReason::ModelNotAllowed,
                    );
                }
                PolicyControlController.authorize(ControlRequest::Model { policy, request })
            }
            query => PolicyControlController.authorize(query),
        }
    }
}

#[derive(Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Profile {
    pub enabled: bool,
    pub level: CaptureLevel,
    pub routes: Vec<ModelRoute>,
    pub release_cpu_on_idle: bool,
}

impl Profile {
    pub fn support(&self) -> Option<GatewaySupport> {
        self.enabled.then(|| GatewaySupport {
            version: CLUSTER_VERSION,
            level: self.level,
            model_patterns: self.routes.iter().map(|r| r.name.clone()).collect(),
        })
    }
    pub fn validate(&self, mode: pvisor::OverlayNetMode) -> anyhow::Result<()> {
        ensure!(
            self.enabled || (self.routes.is_empty() && !self.release_cpu_on_idle),
            "Gateway routes and cooperative CPU release require gateway.enabled"
        );
        if !self.enabled {
            return Ok(());
        }
        ensure!(
            cfg!(feature = "gateway"),
            "Gateway profile requires a build with the gateway feature"
        );
        ensure!(
            mode != pvisor::OverlayNetMode::Off,
            "Gateway requires an enabled network driver"
        );
        self.support().expect("enabled").validate()?;
        for route in &self.routes {
            ensure!(
                route.api_key.is_none(),
                "Worker Gateway credentials require api_key_env, not inline api_key"
            );
        }
        #[cfg(feature = "gateway")]
        {
            self.proxy("profile-validation").validate()?;
            for route in &self.routes {
                // Check declared keys now, without copying their values into
                // the public capability or per-Attempt RunSpec.
                ensure!(
                    route.api_key_env.is_none()
                        || pvisor_gateway::config::api_key_value(route)?.is_some(),
                    "Worker Gateway route has an unresolved api_key_env"
                );
            }
        }
        Ok(())
    }
    #[cfg(feature = "gateway")]
    fn proxy(&self, agent: &str) -> pvisor_gateway::config::ProxyConfig {
        pvisor_gateway::config::ProxyConfig {
            // Actual bind(0), not probe-then-bind: independent concurrent Runs
            // cannot collide on fixed data/admin ports.
            listen: "127.0.0.1:0".into(),
            admin_listen: "127.0.0.1:0".into(),
            agent_id: agent.into(),
            session_header: "x-pvisor-session-id".into(),
            capture_level: self.level,
            debug: false,
            network: Default::default(),
            overlay: Default::default(),
            models: self.routes.clone(),
        }
    }
}

pub(super) fn attach(
    builder: pvisor::PVisorBuilder,
    profile: &Profile,
    task: &TaskSpec,
    storage: &Path,
    #[cfg(feature = "gateway")] model_wait: Option<
        Arc<dyn pvisor_gateway::model_wait::ModelWaitLifecycle>,
    >,
) -> anyhow::Result<pvisor::PVisorBuilder> {
    let Some(requirement) = &task.gateway else {
        return Ok(builder);
    };
    task.validate_gateway()?;
    ensure!(
        profile
            .support()
            .is_some_and(|support| support.satisfies(requirement)),
        "Worker cannot satisfy required Gateway models/capture level"
    );
    #[cfg(feature = "gateway")]
    {
        let mut driver = pvisor::GatewayDriverConfig::new(profile.proxy(&task.run.agent.name))
            .output_dir(storage);
        if let Some(lifecycle) = model_wait {
            driver = driver.model_wait(lifecycle);
        }
        Ok(builder
            .gateway(driver)
            .control_controller(Arc::new(ModelController {
                run_id: task.run.run_id.clone(),
                allowed_models: task.run.capabilities.models.clone(),
            })))
    }
    #[cfg(not(feature = "gateway"))]
    {
        let _ = (builder, storage);
        anyhow::bail!("Gateway requirement needs a build with the gateway feature")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn enabled() -> Profile {
        Profile {
            enabled: true,
            routes: vec![ModelRoute {
                name: "test-*".into(),
                provider: None,
                upstream: Some("http://127.0.0.1:1/v1".into()),
                upstream_anthropic: None,
                api_key: None,
                api_key_env: None,
                forward: None,
            }],
            ..Default::default()
        }
    }
    #[cfg(feature = "gateway")]
    #[test]
    fn model_authorization_requires_task_identity_and_client_alias_even_after_forwarding() {
        use pvisor_core::{ControlController, ControlRequest, ModelAccessPolicy, ModelCallRequest};
        let controller = ModelController {
            run_id: "agent-run".into(),
            allowed_models: vec!["allowed-*".into()],
        };
        let mut policy = ModelAccessPolicy {
            allowed_models: vec!["*".into()],
            allowed_providers: vec!["openai".into()],
        };
        let mut request = ModelCallRequest {
            run_id: Some("agent-run".into()),
            attempt_id: None,
            call_id: "call".into(),
            client_model: "allowed-alias".into(),
            upstream_model: "provider-model".into(),
            provider: "openai".into(),
            protocol: "openai".into(),
            upstream_host: "models.example".into(),
        };
        assert!(
            controller
                .authorize(ControlRequest::Model {
                    policy: &policy,
                    request: &request
                })
                .is_allowed()
        );
        request.client_model = "forbidden-alias".into();
        request.upstream_model = "allowed-upstream".into();
        assert!(
            !controller
                .authorize(ControlRequest::Model {
                    policy: &policy,
                    request: &request
                })
                .is_allowed()
        );
        request.client_model = "allowed-alias".into();
        request.run_id = Some("another-task".into());
        assert!(
            !controller
                .authorize(ControlRequest::Model {
                    policy: &policy,
                    request: &request
                })
                .is_allowed()
        );
        request.run_id = Some("agent-run".into());
        policy.allowed_models.clear();
        assert!(
            !controller
                .authorize(ControlRequest::Model {
                    policy: &policy,
                    request: &request
                })
                .is_allowed()
        );
        policy.allowed_models.push("*".into());
        request.provider = "unauthorized-provider".into();
        assert!(
            !controller
                .authorize(ControlRequest::Model {
                    policy: &policy,
                    request: &request
                })
                .is_allowed()
        );
    }
    #[test]
    fn disabled_profile_preserves_legacy_and_enabled_profile_requires_compiled_driver() {
        assert!(Profile::default().support().is_none());
        Profile::default()
            .validate(pvisor::OverlayNetMode::Off)
            .unwrap();
        let profile = enabled();
        let result = profile.validate(pvisor::OverlayNetMode::Auto);
        assert_eq!(result.is_ok(), cfg!(feature = "gateway"));
        assert!(profile.validate(pvisor::OverlayNetMode::Off).is_err());
        let mut disabled = profile.clone();
        disabled.enabled = false;
        assert!(disabled.validate(pvisor::OverlayNetMode::Auto).is_err());
    }
    #[cfg(feature = "gateway")]
    #[test]
    fn profile_rejects_inline_secrets_invalid_routes_and_missing_declared_keys() {
        let mut profile = enabled();
        profile.routes[0].api_key = Some("not-exportable".into());
        assert!(profile.validate(pvisor::OverlayNetMode::Auto).is_err());
        profile.routes[0].api_key = None;
        profile.routes[0].api_key_env = Some(format!(
            "PVISOR_MISSING_TEST_KEY_{}",
            uuid::Uuid::new_v4().simple()
        ));
        assert!(profile.validate(pvisor::OverlayNetMode::Auto).is_err());
        profile.routes[0].api_key_env = None;
        profile.routes[0].forward = Some("missing-model".into());
        assert!(profile.validate(pvisor::OverlayNetMode::Auto).is_err());
        let capability = enabled().support().unwrap();
        let wire = serde_json::to_string(&capability).unwrap();
        assert!(!wire.contains("127.0.0.1") && !wire.contains("api_key"));
    }
}
