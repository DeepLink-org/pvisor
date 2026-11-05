use super::super::{FilesystemAccessArg, FilesystemLevel};

pub(super) fn patch() -> Vec<FilesystemAccessArg> {
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
        args.push(FilesystemAccessArg {
            path: pattern.into(),
            level: FilesystemLevel::Deny,
        });
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
        args.push(FilesystemAccessArg {
            path: pattern.into(),
            level: FilesystemLevel::Warn,
        });
    }
    args
}
