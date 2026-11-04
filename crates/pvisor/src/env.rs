//! Private environment names and decoding, not runtime policy or resource ownership.
//!
//! Text settings distinguish absence from non-Unicode values. Native paths retain
//! their OS representation; presence-only switches accept any OS value.

use std::ffi::OsString;

/// Decoding failures contain the setting name, never its potentially secret value.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ReadError {
    #[error("environment variable {0} is not present")]
    Missing(String),
    #[error("environment variable {0} must be Unicode")]
    NonUnicode(String),
}

pub(crate) fn unicode(name: &str) -> Result<Option<String>, ReadError> {
    optional_unicode(name, std::env::var(name))
}

fn optional_unicode(
    name: &str,
    value: Result<String, std::env::VarError>,
) -> Result<Option<String>, ReadError> {
    match value {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(ReadError::NonUnicode(name.to_owned())),
    }
}

pub(crate) fn required_unicode(name: &str) -> Result<String, ReadError> {
    unicode(name)?.ok_or_else(|| ReadError::Missing(name.to_owned()))
}

pub(crate) fn os(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

pub(crate) mod cache {
    pub const BACKEND_ENV: &str = "PVISOR_CACHE_BACKEND";
    pub const LOCATION_ENV: &str = "PVISOR_CACHE_LOCATION";
    pub const READ_ONLY_ENV: &str = "PVISOR_CACHE_READ_ONLY";
    pub const TOKEN_ENV: &str = "PVISOR_CACHE_TOKEN";
    pub const SERVER_ENV: &str = "PVISOR_CACHE_SERVER";
    pub const IMAGE_STORE_ENV: &str = "PVISOR_IMAGE_STORE";
    pub const DOCKER_HOME_ENV: &str = "PVISOR_DOCKER_HOME";
    pub(crate) const REUSE_LAZY_NODES_ENV: &str = "PVISOR_VM_FS_REUSE_LAZY_NODES";
    pub(crate) const STREAM_LAZY_READS_ENV: &str = "PVISOR_VM_FS_STREAM_LAZY_READS";
    pub(crate) const DIRECT_LAZY_COPY_ENV: &str = "PVISOR_VM_FS_DIRECT_LAZY_COPY";

    pub(crate) const SOURCE_ENV: [&str; 5] = [
        BACKEND_ENV,
        LOCATION_ENV,
        SERVER_ENV,
        READ_ONLY_ENV,
        TOKEN_ENV,
    ];

    pub(crate) fn explicit() -> bool {
        SOURCE_ENV.iter().any(|key| super::os(key).is_some())
    }

    pub(crate) fn experiment_setting(
        name: &str,
        setting: Option<&std::ffi::OsStr>,
    ) -> anyhow::Result<bool> {
        match setting {
            None => Ok(false),
            Some(value) if value == "0" => Ok(false),
            Some(value) if value == "1" => Ok(true),
            Some(_) => anyhow::bail!("{name} must be 0 or 1"),
        }
    }
}

pub(crate) mod ram {
    pub(crate) const POOL_ENV: &str = "PVISOR_EXPERIMENTAL_MEMORY_POOL";
    pub(crate) const METRICS_ENV: &str = "PVISOR_EXPERIMENTAL_MEMORY_METRICS";
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    pub(crate) const PAGE_INVENTORY_ENV: &str = "PVISOR_EXPERIMENTAL_MEMORY_PAGE_INVENTORY";
    #[cfg(target_os = "macos")]
    pub(crate) const PROCESS_INVENTORY_ENV: &str = "PVISOR_EXPERIMENTAL_MEMORY_PROCESS_INVENTORY";
}

pub(crate) mod runner {
    pub(crate) const RUNNER_SPEC_ENV: &str = "PVISOR_KRUN_RUNNER_SPEC";
    pub(crate) const NETWORK_FD_ENV: &str = "PVISOR_KRUN_NETWORK_FD";
    pub(crate) const CONTROL_FD_ENV: &str = "PVISOR_KRUN_CONTROL_FD";
    pub(crate) const RAM_FD_ENV: &str = "PVISOR_KRUN_RAM_FD";
    pub(crate) const READONLY_FLUSH_ENV: &str = "PVISOR_VM_FS_READONLY_FLUSH_ELISION";
    pub(crate) const LOGICAL_DIRECTORY_CACHE_ENV: &str = "PVISOR_VM_FS_LOGICAL_DIRECTORY_CACHE";
    pub(crate) const IDLE_FD_ENV: &str = "PVISOR_KRUN_IDLE_FD";
    pub(crate) const RAW_READY_FD_ENV: &str = "PVISOR_KRUN_RAW_READY_FD";
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub(crate) const ENOMEM_OVERRIDE_ENV: &str = "PVISOR_KRUN_ENOMEM_WORKAROUND";
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub(crate) const ENOMEM_WORKAROUND_ENV: &str = "KRUN_ENOMEM_WORKAROUND";
    pub(crate) const RESTORE_RAM_WATCHDOG_ENV: &str = "PVISOR_VM_RESTORE_RAM_WATCHDOG";
    pub(crate) const RESTORE_RAM_SERVER_ENV: &str = "PVISOR_VM_RESTORE_RAM_SERVER";
    pub(crate) const LOADER_ENV: [&str; 3] = [
        "LD_LIBRARY_PATH",
        "DYLD_LIBRARY_PATH",
        "DYLD_FALLBACK_LIBRARY_PATH",
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_text_distinguishes_missing_empty_and_non_unicode() {
        assert_eq!(
            optional_unicode(cache::TOKEN_ENV, Err(std::env::VarError::NotPresent)),
            Ok(None)
        );
        assert_eq!(
            optional_unicode(cache::TOKEN_ENV, Ok(String::new())),
            Ok(Some(String::new()))
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let value = OsString::from_vec(b"sensitive-token-\xff".to_vec());
            let error =
                optional_unicode(cache::TOKEN_ENV, Err(std::env::VarError::NotUnicode(value)))
                    .unwrap_err();
            assert_eq!(
                error.to_string(),
                "environment variable PVISOR_CACHE_TOKEN must be Unicode"
            );
            assert!(!format!("{error:?}").contains("sensitive-token"));
        }
    }
}
