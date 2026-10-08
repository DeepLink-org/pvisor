pub(super) fn patch(audit: bool) -> Vec<String> {
    let mut args = Vec::new();
    for pattern in [
        "**/.ssh",
        "**/.gnupg",
        "**/id_rsa",
        "**/id_dsa",
        "**/id_ecdsa",
        "**/id_ecdsa_sk",
        "**/id_ed25519",
        "**/id_ed25519_sk",
    ] {
        args.extend(["--access".into(), format!("{pattern}:deny")]);
    }
    for pattern in [
        "**/.env",
        "**/.env.*",
        "**/*.pem",
        "**/*.key",
        "**/*.pub",
        "**/*.p12",
        "**/*.pfx",
        "**/.aws/credentials",
        "**/.netrc",
        "**/.npmrc",
    ] {
        args.extend([
            "--access".into(),
            format!("{pattern}:{}", if audit { "ask" } else { "warn" }),
        ]);
    }
    args
}
