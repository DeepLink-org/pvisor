pub(super) fn patch() -> Vec<String> {
    let mut args = vec!["--overlayfs-commit".into(), "manual".into()];
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
        args.extend(["--overlayfs-deny".into(), pattern.into()]);
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
        args.extend(["--overlayfs-warn".into(), pattern.into()]);
    }
    args
}
