use anyhow::{bail, ensure};
use std::path::{Component, Path};

use super::{http::Http, storage::Storage};

pub(super) enum Repository<'a> {
    File(&'a Path),
    S3(&'a str),
    Gcs { bucket: &'a str, prefix: &'a str },
    Http(&'a str),
}

impl<'a> Repository<'a> {
    pub(super) fn parse(address: &'a str) -> anyhow::Result<Self> {
        if let Some(path) = address.strip_prefix("file://") {
            let local = Path::new(path);
            ensure!(
                local.is_absolute()
                    && !path.starts_with("//")
                    && !path.contains(['?', '#', '%', '\\', '\0'])
                    && !local.components().any(|part| part == Component::ParentDir),
                "filesystem repository requires file:///ABSOLUTE/PATH"
            );
            Ok(Self::File(local))
        } else if address.starts_with("s3://") {
            super::storage::s3_location(address)?;
            Ok(Self::S3(address))
        } else if let Some(path) = address.strip_prefix("gs://") {
            let (bucket, prefix) = path.split_once('/').unwrap_or((path, ""));
            ensure!(
                !bucket.is_empty()
                    && bucket.bytes().all(|b| b.is_ascii_lowercase()
                        || b.is_ascii_digit()
                        || b"._-".contains(&b))
                    && !matches!(bucket, "." | ".."),
                "invalid Google Cloud Storage bucket"
            );
            let prefix = prefix.trim_end_matches('/');
            if !prefix.is_empty() {
                super::storage::validate_key(prefix)?;
            }
            Ok(Self::Gcs { bucket, prefix })
        } else if address.starts_with("http://") || address.starts_with("https://") {
            Http::validate(address)?;
            Ok(Self::Http(address))
        } else {
            bail!("repository requires file:///, s3://, gs://, http:// or https://")
        }
    }

    pub(super) fn from_address(address: &'a str) -> anyhow::Result<Option<Self>> {
        if ["file://", "s3://", "gs://", "http://", "https://"]
            .iter()
            .any(|scheme| address.starts_with(scheme))
        {
            Self::parse(address).map(Some)
        } else {
            Ok(None)
        }
    }

    pub(super) fn validate_writable(&self) -> anyhow::Result<()> {
        ensure!(
            !matches!(self, Self::Http(_)),
            "HTTP repositories are read-only"
        );
        Ok(())
    }

    pub(super) fn open(&self, read_only: bool) -> anyhow::Result<Storage> {
        if !read_only {
            self.validate_writable()?;
        }
        match self {
            Self::File(path) => Storage::filesystem((*path).into(), !read_only),
            Self::S3(address) => Storage::s3(address),
            Self::Gcs { bucket, prefix } => super::opendal_store::gcs(bucket, prefix, read_only),
            Self::Http(address) => Ok(Storage::Http(std::sync::Arc::new(Http::new(address)?))),
        }
    }
}
