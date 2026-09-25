pub(super) fn patch() -> Vec<String> {
    let mut args = Vec::new();
    for host in ["api.openai.com", "chatgpt.com", "ab.chatgpt.com"] {
        args.extend(["--overlaynet-allow".into(), format!("{host}:443")]);
    }
    // Codex keeps its SQLite state and routing cache below CODEX_HOME (or
    // ~/.codex). Stage that directory by default so safe Runs can initialize
    // and update state without mutating the user's live database.
    let path = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".codex"))
        });
    if let Some(path) = path {
        args.extend(["--mount".into(), format!("{}:stage", path.display())]);
    }
    args
}
