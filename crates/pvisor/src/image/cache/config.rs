//! Shared CLI and executor configuration for server and daemonless caches.
use super::{SERVER_ENV, default_endpoint};
use anyhow::{Context, ensure};
use clap::ValueEnum;
use std::path::PathBuf;

pub const BACKEND_ENV: &str = "PVISOR_CACHE_BACKEND";
pub const LOCATION_ENV: &str = "PVISOR_CACHE_LOCATION";
pub const READ_ONLY_ENV: &str = "PVISOR_CACHE_READ_ONLY";
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum CacheBackend {
    #[default]
    Server,
    Filesystem,
    S3,
}
#[derive(Clone, Debug)]
pub struct CacheConfig {
    pub backend: CacheBackend,
    pub location: String,
    pub read_only: bool,
    pub image_store: Option<PathBuf>,
}
impl CacheConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_options(None, None, None, None)
    }
    pub(crate) fn from_options(
        backend: Option<CacheBackend>,
        location: Option<String>,
        read_only: Option<bool>,
        image_store: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let backend = match backend {
            Some(backend) => backend,
            None => match std::env::var(BACKEND_ENV) {
                Ok(value) => CacheBackend::from_str(&value, false).map_err(anyhow::Error::msg)?,
                Err(std::env::VarError::NotPresent) => CacheBackend::Server,
                Err(error) => return Err(error.into()),
            },
        };
        let location = match location {
            Some(location) => location,
            None => match std::env::var(LOCATION_ENV) {
                Ok(value) => value,
                Err(std::env::VarError::NotPresent) if backend == CacheBackend::Server => {
                    match std::env::var(SERVER_ENV) {
                        Ok(value) => value,
                        Err(std::env::VarError::NotPresent) => default_endpoint()?,
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(std::env::VarError::NotPresent) => {
                    anyhow::bail!("{LOCATION_ENV} is required for {backend:?}")
                }
                Err(error) => return Err(error.into()),
            },
        };
        let read_only = match read_only {
            Some(read_only) => read_only,
            None => match std::env::var(READ_ONLY_ENV) {
                Ok(value) => match value.as_str() {
                    "1" | "true" => true,
                    "0" | "false" => false,
                    _ => anyhow::bail!("{READ_ONLY_ENV} must be true/false or 1/0"),
                },
                Err(std::env::VarError::NotPresent) => false,
                Err(error) => return Err(error.into()),
            },
        };
        Ok(Self {
            backend,
            location,
            read_only,
            image_store: image_store
                .or_else(|| std::env::var_os("PVISOR_IMAGE_STORE").map(PathBuf::from)),
        })
    }
    pub(super) fn address(&self) -> anyhow::Result<String> {
        match self.backend {
            CacheBackend::Server => Ok(self.location.clone()),
            CacheBackend::Filesystem => {
                let path = self
                    .location
                    .strip_prefix("file://")
                    .unwrap_or(&self.location);
                ensure!(
                    std::path::Path::new(path).is_absolute(),
                    "filesystem cache location must be absolute"
                );
                Ok(format!("file://{path}"))
            }
            CacheBackend::S3 => {
                self.location
                    .strip_prefix("s3://")
                    .context("S3 cache requires s3://BUCKET/PREFIX")?;
                Ok(self.location.clone())
            }
        }
    }
    pub(super) fn explicit() -> bool {
        [BACKEND_ENV, LOCATION_ENV, SERVER_ENV]
            .iter()
            .any(|key| std::env::var_os(key).is_some())
    }
}

/// Cache configuration and implicit storage credentials belong to the host.
/// Explicit invocation environment is merged by the executor after this step.
pub(crate) fn scrub_guest_environment(env: &mut std::collections::BTreeMap<String, String>) {
    let s3 = env.get(BACKEND_ENV).is_some_and(|value| value == "s3")
        || [LOCATION_ENV, SERVER_ENV].iter().any(|key| {
            env.get(*key)
                .is_some_and(|value| value.starts_with("s3://"))
        });
    for key in [
        BACKEND_ENV,
        LOCATION_ENV,
        READ_ONLY_ENV,
        SERVER_ENV,
        "PVISOR_CACHE_TOKEN",
    ] {
        env.remove(key);
    }
    if s3 {
        env.retain(|key, _| !key.starts_with("AWS_"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    #[test]
    fn all_s3_selection_forms_scrub_implicit_storage_credentials() {
        for (key, value) in [
            (BACKEND_ENV, "s3"),
            (LOCATION_ENV, "s3://bucket/prefix"),
            (SERVER_ENV, "s3://bucket/prefix"),
        ] {
            let mut env = BTreeMap::from([
                (key.into(), value.into()),
                ("AWS_ACCESS_KEY_ID".into(), "test-key".into()),
                ("AWS_SECRET_ACCESS_KEY".into(), "test-secret".into()),
                ("AWS_SESSION_TOKEN".into(), "test-session".into()),
                (
                    "AWS_WEB_IDENTITY_TOKEN_FILE".into(),
                    "/private/token".into(),
                ),
                ("PATH".into(), "/usr/bin".into()),
            ]);
            scrub_guest_environment(&mut env);
            assert_eq!(env, BTreeMap::from([("PATH".into(), "/usr/bin".into())]));
        }
        let mut normal = BTreeMap::from([("AWS_ACCESS_KEY_ID".into(), "agent-key".into())]);
        scrub_guest_environment(&mut normal);
        assert!(
            normal.contains_key("AWS_ACCESS_KEY_ID"),
            "ordinary explicit AWS workflows are unchanged"
        );
    }
}
