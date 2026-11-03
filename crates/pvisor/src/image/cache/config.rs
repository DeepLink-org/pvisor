//! Shared CLI and executor configuration for server and daemonless caches.
use super::default_endpoint;
use crate::env::{
    self,
    cache::{IMAGE_STORE_ENV, SERVER_ENV},
};
use anyhow::ensure;
use std::path::PathBuf;

pub use crate::env::cache::{BACKEND_ENV, LOCATION_ENV, READ_ONLY_ENV};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CacheBackend {
    #[default]
    Server,
    Filesystem,
    S3,
    /// Public static HTTP/HTTPS V2 repository; all mutations are forbidden.
    Http,
}
impl std::str::FromStr for CacheBackend {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "server" => Ok(Self::Server),
            "filesystem" => Ok(Self::Filesystem),
            "s3" => Ok(Self::S3),
            "http" | "https" => Ok(Self::Http),
            _ => Err(format!(
                "invalid cache backend {value:?}; expected server, filesystem, s3, http"
            )),
        }
    }
}
#[derive(Clone, Debug)]
pub struct CacheConfig {
    pub backend: CacheBackend,
    pub location: String,
    pub read_only: bool,
    pub image_store: Option<PathBuf>,
}

/// Validated host-side capability; never serialized in runner descriptors.
pub(super) enum ResolvedSource {
    Pool(PathBuf),
    Filesystem(PathBuf),
    S3,
    Http,
    Server(super::transport::Endpoint),
}

impl ResolvedSource {
    pub(super) fn resolve(
        address: &str,
        read_only: bool,
        token: Option<&str>,
    ) -> anyhow::Result<Self> {
        for (prefix, pool) in [("pool+file://", true), ("file://", false)] {
            if let Some(path) = address.strip_prefix(prefix) {
                ensure!(
                    std::path::Path::new(path).is_absolute(),
                    "cache path must be absolute"
                );
                return Ok(if pool {
                    Self::Pool(path.into())
                } else {
                    Self::Filesystem(path.into())
                });
            }
        }
        if address.starts_with("s3://") {
            super::storage::s3_location(address)?;
            return Ok(Self::S3);
        }
        if address.starts_with("ipfs+")
            || address.starts_with("http://")
            || address.starts_with("https://")
        {
            ensure!(read_only, "HTTP/IPFS cache must be read-only");
            super::http::Http::validate(address)?;
            return Ok(Self::Http);
        }
        ensure!(!read_only, "read-only mode requires an object repository");
        let endpoint = super::transport::endpoint(address)?;
        if matches!(endpoint, super::transport::Endpoint::Tcp(_)) {
            ensure!(
                token.is_some_and(|value| !value.is_empty()),
                "TCP requires PVISOR_CACHE_TOKEN"
            );
        }
        Ok(Self::Server(endpoint))
    }

    pub(super) fn needs_host_network(&self) -> bool {
        matches!(
            self,
            Self::S3 | Self::Http | Self::Server(super::transport::Endpoint::Tcp(_))
        )
    }
}

impl CacheConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_options(None, None, None, None)
    }
    pub fn from_options(
        backend: Option<CacheBackend>,
        location: Option<String>,
        read_only: Option<bool>,
        image_store: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let backend = match backend {
            Some(backend) => backend,
            None => match env::unicode(BACKEND_ENV)? {
                Some(value) => value.parse::<CacheBackend>().map_err(anyhow::Error::msg)?,
                None => CacheBackend::Server,
            },
        };
        let location = match location {
            Some(location) => location,
            None => match env::unicode(LOCATION_ENV)? {
                Some(value) => value,
                None if backend == CacheBackend::Server => match env::unicode(SERVER_ENV)? {
                    Some(value) => value,
                    None => default_endpoint()?,
                },
                None => {
                    anyhow::bail!("{LOCATION_ENV} is required for {backend:?}")
                }
            },
        };
        let read_only = match read_only {
            Some(read_only) => read_only,
            None => match env::unicode(READ_ONLY_ENV)? {
                Some(value) => match value.as_str() {
                    "1" | "true" => true,
                    "0" | "false" => false,
                    _ => anyhow::bail!("{READ_ONLY_ENV} must be true/false or 1/0"),
                },
                None => backend == CacheBackend::Http,
            },
        };
        Ok(Self {
            backend,
            location,
            read_only,
            image_store: image_store.or_else(|| env::os(IMAGE_STORE_ENV).map(PathBuf::from)),
        })
    }
    pub fn address(&self) -> anyhow::Result<String> {
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
                super::storage::s3_location(&self.location)?;
                Ok(self.location.clone())
            }
            CacheBackend::Http => {
                ensure!(self.read_only, "HTTP repositories are read-only");
                ensure!(
                    self.location.starts_with("http://") || self.location.starts_with("https://"),
                    "HTTP repository requires http:// or https://"
                );
                super::http::Http::validate(&self.location)?;
                Ok(self.location.clone())
            }
        }
    }
    pub(super) fn explicit() -> bool {
        env::cache::explicit()
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
    for key in env::cache::SOURCE_ENV {
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
    fn resolved_source_validates_capabilities_before_storage_io() {
        for address in [
            "file://relative",
            "pool+file://relative",
            "unix://relative",
            "tcp://192.0.2.1:1234",
            "unknown://source",
        ] {
            assert!(
                ResolvedSource::resolve(address, false, Some("token")).is_err(),
                "{address}"
            );
        }
        assert!(ResolvedSource::resolve("tcp://127.0.0.1:1234", false, None).is_err());
        assert!(ResolvedSource::resolve("https://mirror.example/root", false, None).is_err());
        for (address, read_only, network) in [
            ("file:///tmp/repository", false, false),
            ("pool+file:///tmp/pool", false, false),
            ("unix:///tmp/cache.sock", false, false),
            ("tcp://127.0.0.1:1234", false, true),
            ("https://mirror.example/root", true, true),
            ("s3://bucket/root", false, true),
        ] {
            let source = ResolvedSource::resolve(address, read_only, Some("token")).unwrap();
            assert_eq!(source.needs_host_network(), network, "{address}");
        }
    }

    #[test]
    fn http_and_s3_repository_options_validate_without_io() {
        for address in ["http://mirror.example/root", "https://mirror.example/root"] {
            let config = CacheConfig::from_options(
                Some(CacheBackend::Http),
                Some(address.into()),
                Some(true),
                None,
            )
            .unwrap();
            assert_eq!(config.address().unwrap(), address);
            let writable = CacheConfig {
                read_only: false,
                ..config
            };
            assert!(writable.address().is_err());
        }
        for address in [
            "s3://",
            "s3://user:secret@bucket/root",
            "s3://bucket/../escape",
            "s3://bucket/root?token=x",
        ] {
            let config = CacheConfig {
                backend: CacheBackend::S3,
                location: address.into(),
                read_only: true,
                image_store: None,
            };
            assert!(config.address().is_err());
        }
        assert_eq!("https".parse::<CacheBackend>().unwrap(), CacheBackend::Http);
    }
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
