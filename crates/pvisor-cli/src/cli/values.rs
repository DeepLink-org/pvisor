//! Clap adapters for runtime configuration enums; the runtime has no parser dependency.
use clap::builder::{PossibleValuesParser, TypedValueParser};

pub(super) fn run_executor_kind() -> impl TypedValueParser<Value = pvisor::RunExecutorKind> {
    PossibleValuesParser::new(["host", "container", "vm"])
        .try_map(|value| serde_json::from_value(serde_json::Value::String(value)))
}

pub(super) fn filesystem_mode() -> impl TypedValueParser<Value = pvisor::FilesystemMode> {
    PossibleValuesParser::new(["host", "sandbox"])
        .try_map(|value| serde_json::from_value(serde_json::Value::String(value)))
}

pub(super) fn run_stdio() -> impl TypedValueParser<Value = pvisor::RunStdio> {
    PossibleValuesParser::new(["inherit", "capture"])
        .try_map(|value| serde_json::from_value(serde_json::Value::String(value)))
}

pub(super) fn container_network() -> impl TypedValueParser<Value = pvisor::ContainerNetwork> {
    PossibleValuesParser::new(["host", "bridge", "none"])
        .try_map(|value| serde_json::from_value(serde_json::Value::String(value)))
}

pub(super) fn overlay_net_mode() -> impl TypedValueParser<Value = pvisor::OverlayNetMode> {
    PossibleValuesParser::new(["auto", "off", "proxy"])
        .try_map(|value| serde_json::from_value(serde_json::Value::String(value)))
}

pub(super) fn overlay_net_policy() -> impl TypedValueParser<Value = pvisor::OverlayNetPolicy> {
    PossibleValuesParser::new(["public", "deny", "allowlist"])
        .try_map(|value| serde_json::from_value(serde_json::Value::String(value)))
}

pub(super) fn gateway_profile() -> impl TypedValueParser<Value = pvisor::GatewayProfile> {
    PossibleValuesParser::new(["zcode-bigmodel"])
        .try_map(|value| serde_json::from_value(serde_json::Value::String(value)))
}

pub(super) fn gateway_mode() -> impl TypedValueParser<Value = pvisor::GatewayMode> {
    PossibleValuesParser::new(["off", "capture"])
        .try_map(|value| serde_json::from_value(serde_json::Value::String(value)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(parser: impl TypedValueParser, names: &[&str]) {
        let command = clap::Command::new("pvisor");
        for name in names {
            parser
                .parse_ref(&command, None, std::ffi::OsStr::new(name))
                .unwrap();
        }
        assert!(
            parser
                .parse_ref(&command, None, std::ffi::OsStr::new("unknown"))
                .is_err()
        );
    }

    #[test]
    fn runtime_enum_adapters_keep_all_cli_choices() {
        check(run_executor_kind(), &["host", "container", "vm"]);
        check(filesystem_mode(), &["host", "sandbox"]);
        check(run_stdio(), &["inherit", "capture"]);
        check(container_network(), &["host", "bridge", "none"]);
        check(overlay_net_mode(), &["auto", "off", "proxy"]);
        check(overlay_net_policy(), &["public", "deny", "allowlist"]);
        check(gateway_profile(), &["zcode-bigmodel"]);
        check(gateway_mode(), &["off", "capture"]);
    }
}
