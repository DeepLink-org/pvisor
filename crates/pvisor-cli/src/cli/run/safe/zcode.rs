pub(super) fn patch() -> Vec<String> {
    // API-key OpenAI-compatible endpoints: https://zcode.z.ai/cn/docs/configuration
    // Keep zcode.z.ai (shared model/business gateway) and object storage outside the grants.
    // Official Anthropic endpoints are rewritten to that gateway in ZCode 872ad960de7e;
    // use the OpenAI-compatible endpoints for this direct-API preset.
    vec![
        "--overlaynet-allow".into(),
        "api.z.ai:443".into(),
        "--overlaynet-allow".into(),
        "open.bigmodel.cn:443".into(),
    ]
}
