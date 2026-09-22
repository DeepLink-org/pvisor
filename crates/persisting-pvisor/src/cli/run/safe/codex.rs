pub(super) fn patch() -> Vec<String> {
    vec!["--overlaynet-allow".into(), "api.openai.com:443".into()]
}
