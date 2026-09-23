//! ZCode's explicit proxy and per-Run BigModel provider catalog adaptation.
//! Credentials and personal model selections remain owned by ZCode.

use std::path::Path;

use anyhow::Context;
use persisting_control::{
    FilesystemAccess, FilesystemCapability, ProcessInvocation, RunInvocation, RunSpec,
};
use serde_json::Value;

use super::ImplantPlan;
use crate::config::GatewayProfile;

const BUILTIN_ENV: &str = "ZCODE_BUILTIN_PROVIDER_CONFIG_FILE";
const PERSONAL_ENV: &str = "ZCODE_PERSONAL_PROVIDER_CONFIG_FILE";
const PROVIDER_ID: &str = "account:bigmodel-individual-coding-plan";

fn is_zcode(process: &ProcessInvocation) -> bool {
    Path::new(&process.program)
        .file_name()
        .and_then(|s| s.to_str())
        == Some("zcode")
}

/// Called only after the network listener has bound its Run-specific port.
pub(super) fn prepare(
    spec: &mut RunSpec,
    plan: &mut ImplantPlan,
    listen: &str,
    storage: &Path,
    profile: Option<GatewayProfile>,
) -> anyhow::Result<()> {
    let RunInvocation::Process(process) = &spec.invocation;
    if !is_zcode(process) {
        anyhow::ensure!(
            profile != Some(GatewayProfile::ZcodeBigmodel),
            "zcode-bigmodel requires a direct zcode command"
        );
        return Ok(());
    }
    plan.env
        .insert("ZCODE_HTTP_PROXY".into(), format!("http://{listen}"));
    plan.env
        .insert("ZCODE_NO_PROXY".into(), "127.0.0.1,localhost".into());
    if profile != Some(GatewayProfile::ZcodeBigmodel) {
        return Ok(());
    }
    let source = spec
        .metadata
        .get("pvisor.gateway.zcode_builtin_config")
        .and_then(Value::as_str)
        .context("zcode-bigmodel requires a built-in provider catalog")?;
    let mut catalog: Value = serde_json::from_slice(
        &std::fs::read(source).context("read ZCode built-in provider catalog")?,
    )?;
    redirect_catalog(&mut catalog, listen)?;
    let personal = if let Some(path) = process.env.get(PERSONAL_ENV) {
        path.clone()
    } else {
        let home = process
            .env
            .get("ZCODE_DATA_BASE_DIR")
            .or_else(|| process.env.get("HOME"))
            .context("ZCode provider adaptation requires HOME or ZCODE_DATA_BASE_DIR")?;
        Path::new(home)
            .join(".zcode/v2/provider_config.json")
            .display()
            .to_string()
    };
    // Only the non-secret built-in catalog is copied. Supplying both paths
    // disables ZCode's catalog refresh for this Run, which would undo routing.
    let directory = storage.join("client-config");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join("zcode-builtin.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&catalog)?)?;
    spec.capabilities.filesystem.push(FilesystemCapability {
        path: directory.display().to_string(),
        access: FilesystemAccess::Read,
    });
    plan.env
        .insert(BUILTIN_ENV.into(), path.display().to_string());
    plan.env.insert(PERSONAL_ENV.into(), personal);
    plan.notes.push(
        "ZCode: Run-local BigModel catalog; existing credentials and personal settings".into(),
    );
    Ok(())
}

fn redirect_catalog(catalog: &mut Value, listen: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        catalog["schemaVersion"] == 1,
        "unsupported ZCode provider catalog schema"
    );
    let rules = catalog
        .pointer_mut("/config/providerConfigRules/providerRules")
        .and_then(Value::as_array_mut)
        .context("ZCode catalog has no provider rules")?;
    let mut matched = 0;
    for rule in rules {
        if rule["providerId"] != PROVIDER_ID {
            continue;
        }
        let api = &mut rule["config"]["api"];
        anyhow::ensure!(
            api["type"] == "anthropic-messages"
                && api["baseUrl"].as_str().is_some_and(
                    |url| url.trim_end_matches('/') == "https://open.bigmodel.cn/api/anthropic"
                ),
            "unsupported BigModel endpoint or protocol in ZCode catalog"
        );
        api["baseUrl"] = format!("http://{listen}/v1").into();
        matched += 1;
    }
    anyhow::ensure!(
        matched == 1,
        "ZCode catalog must contain exactly one BigModel individual provider"
    );
    Ok(())
}

pub(super) fn apply_environment(process: &mut ProcessInvocation, plan: &ImplantPlan) {
    if !is_zcode(process) {
        return;
    }
    // The Run's listener must replace a passed host proxy, otherwise ZCode
    // silently bypasses OverlayNet while standard clients use the listener.
    for key in [
        "ZCODE_HTTP_PROXY",
        "ZCODE_NO_PROXY",
        BUILTIN_ENV,
        PERSONAL_ENV,
    ] {
        if let Some(value) = plan.env.get(key) {
            process.env.insert(key.into(), value.clone());
        }
    }
    if plan.env.contains_key(BUILTIN_ENV) {
        process
            .env
            .remove("ZCODE_BUILTIN_PROVIDER_BUNDLED_CONFIG_FILE");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog() -> Value {
        json!({"schemaVersion":1,"revision":30,"config":{"providerConfigRules":{
        "providerRules":[
            {"providerId":PROVIDER_ID,"config":{"access":{"type":"zhipu-account"},
                "api":{"type":"anthropic-messages","baseUrl":"https://open.bigmodel.cn/api/anthropic"}}},
            {"providerId":"unrelated","config":{"api":{"baseUrl":"https://example.com/v1"}}}
        ]}}})
    }

    #[test]
    fn catalog_redirect_preserves_identity_and_other_providers() {
        let mut value = catalog();
        let original = value.clone();
        redirect_catalog(&mut value, "127.0.0.1:49231").unwrap();
        let rules = &value["config"]["providerConfigRules"]["providerRules"];
        assert_eq!(
            rules[0]["config"]["api"]["baseUrl"],
            "http://127.0.0.1:49231/v1"
        );
        assert_eq!(rules[0]["providerId"], PROVIDER_ID);
        assert_eq!(
            rules[0]["config"]["access"],
            original["config"]["providerConfigRules"]["providerRules"][0]["config"]["access"]
        );
        assert_eq!(
            rules[1],
            original["config"]["providerConfigRules"]["providerRules"][1]
        );
    }

    #[test]
    fn unknown_catalog_and_endpoint_fail_instead_of_silently_bypassing() {
        assert!(redirect_catalog(&mut json!({"schemaVersion":2}), "127.0.0.1:1").is_err());
        let mut value = catalog();
        value["config"]["providerConfigRules"]["providerRules"][0]["config"]["api"]["baseUrl"] =
            "https://other.example".into();
        assert!(redirect_catalog(&mut value, "127.0.0.1:1").is_err());
    }

    #[test]
    fn run_proxy_replaces_passed_host_proxy_only_for_zcode() {
        let mut spec = RunSpec::process("run-test", "zcode", "/opt/bin/zcode");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process
            .env
            .insert("ZCODE_HTTP_PROXY".into(), "http://external:7890".into());
        process.env.insert("ZCODE_NO_PROXY".into(), "*".into());
        let mut plan = ImplantPlan::default();
        prepare(
            &mut spec,
            &mut plan,
            "127.0.0.1:49231",
            Path::new("/unused"),
            None,
        )
        .unwrap();
        let RunInvocation::Process(process) = &mut spec.invocation;
        apply_environment(process, &plan);
        assert_eq!(process.env["ZCODE_HTTP_PROXY"], "http://127.0.0.1:49231");
        assert_eq!(process.env["ZCODE_NO_PROXY"], "127.0.0.1,localhost");
        assert!(!process.env.contains_key(BUILTIN_ENV));
    }

    #[test]
    fn snapshot_is_run_local_and_original_catalog_is_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("installed.json");
        let original = serde_json::to_vec(&catalog()).unwrap();
        std::fs::write(&source, &original).unwrap();
        let mut spec = RunSpec::process("run-test", "zcode", "zcode");
        spec.metadata.insert(
            "pvisor.gateway.zcode_builtin_config".into(),
            source.display().to_string().into(),
        );
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.env.insert("HOME".into(), "/example/home".into());
        let mut plan = ImplantPlan::default();
        prepare(
            &mut spec,
            &mut plan,
            "127.0.0.1:49231",
            &root.path().join("run"),
            Some(GatewayProfile::ZcodeBigmodel),
        )
        .unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(
            plan.env[PERSONAL_ENV],
            "/example/home/.zcode/v2/provider_config.json"
        );
        assert!(Path::new(&plan.env[BUILTIN_ENV]).is_file());
        assert_eq!(spec.capabilities.filesystem.len(), 1);
        assert_eq!(
            spec.capabilities.filesystem[0].access,
            FilesystemAccess::Read
        );
    }
}
