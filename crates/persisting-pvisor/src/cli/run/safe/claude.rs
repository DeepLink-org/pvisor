pub(super) fn patch() -> Vec<String> {
    vec!["--overlaynet-allow".into(), "api.anthropic.com:443".into()]
}
