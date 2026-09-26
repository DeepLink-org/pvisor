pub(super) fn patch() -> Vec<String> {
    let mut args = Vec::new();
    for host in ["api.openai.com", "chatgpt.com", "ab.chatgpt.com"] {
        args.extend(["--overlaynet-allow".into(), format!("{host}:443")]);
    }
    args
}
