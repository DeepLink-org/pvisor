//! Private host environment parsing; no execution resources live here.

#[cfg(unix)]
pub(crate) mod fs {
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::thread;

    use crate::devices::virtio::fs::write_buffer::WriteBufferOptions;

    pub(crate) const WORKERS: &str = "PVISOR_VM_FS_WORKERS";
    pub(crate) const CACHE_EXPERIMENT: &str = "PVISOR_VM_CACHE_EXPERIMENT";
    pub(crate) const GUEST_CACHE: &str = "PVISOR_VM_GUEST_CACHE";
    pub(crate) const HOST_WRITEBACK: &str = "PVISOR_VM_FS_HOST_WRITEBACK";
    pub(crate) const HOST_WRITEBACK_CONFIG: &str = "PVISOR_VM_FS_HOST_WRITEBACK_CONFIG";
    pub(crate) const DIRECTORY_CACHE_CAPACITY: &str = "PVISOR_VM_FS_DIRECTORY_CACHE_CAPACITY";
    pub(crate) const EXCLUSIVE_UPPER_ABSENCE_CACHE: &str =
        "PVISOR_VM_FS_EXCLUSIVE_UPPER_ABSENCE_CACHE";
    pub(crate) const READONLY_FLUSH_ELISION: &str = "PVISOR_VM_FS_READONLY_FLUSH_ELISION";
    pub(crate) const LOWER_DATA_CACHE: &str = "PVISOR_VM_FS_LOWER_DATA_CACHE";
    pub(crate) const NEGATIVE_ENTRY_CACHE: &str = "PVISOR_VM_FS_NEGATIVE_ENTRY_CACHE";
    pub(crate) const WRITEBACK: &str = "PVISOR_VM_FS_WRITEBACK";
    pub(crate) const SHARED_OPEN_QUALIFICATION: &str = "PVISOR_VM_FS_SHARED_OPEN_QUALIFICATION";
    pub(crate) const DIRECTORY_NAMES_CACHE: &str = "PVISOR_VM_FS_DIRECTORY_NAMES_CACHE";
    pub(crate) const PREPARED_COPY_UP: &str = "PVISOR_VM_FS_PREPARED_COPY_UP";
    pub(crate) const OBJECT_READ_BARRIERS: &str = "PVISOR_VM_FS_OBJECT_READ_BARRIERS";
    pub(crate) const LOGICAL_DIRECTORY_CACHE: &str = "PVISOR_VM_FS_LOGICAL_DIRECTORY_CACHE";
    pub(crate) const SCOPED_NAMES: &str = "PVISOR_VM_FS_SCOPED_NAMES";
    pub(crate) const LOGICAL_DATA_CACHE: &str = "PVISOR_VM_FS_LOGICAL_DATA_CACHE";
    pub(crate) const DEFAULT_DIRECTORY_CACHE_CAPACITY: usize = 256;
    pub(crate) const MAX_DIRECTORY_CACHE_CAPACITY: usize = 4096;

    /// Captured once before publishing any device. Shared-open errors are deferred
    /// until overlay construction; other backends do not consume that setting.
    pub(crate) struct FsProfile {
        pub(crate) workers: usize,
        pub(crate) root_data_cache: bool,
        pub(crate) guest_cache: bool,
        pub(crate) host_writeback: bool,
        pub(crate) host_writeback_config: HostWritebackConfig,
        pub(crate) directory_cache_capacity: usize,
        pub(crate) exclusive_upper_absence_cache: bool,
        pub(crate) readonly_flush_elision: bool,
        pub(crate) lower_data_cache: bool,
        pub(crate) negative_entry_cache: bool,
        pub(crate) directory_names_cache: bool,
        pub(crate) prepared_copy_up: bool,
        pub(crate) object_read_barriers: bool,
        pub(crate) logical_directory_cache: bool,
        pub(crate) scoped_names: bool,
        pub(crate) logical_data_cache: bool,
        pub(crate) writeback: Option<String>,
        pub(crate) shared_open: io::Result<bool>,
    }

    impl FsProfile {
        pub(crate) fn from_env() -> io::Result<Self> {
            Self::parse(|key| std::env::var_os(key))
        }

        pub(crate) fn parse(
            mut get: impl FnMut(&str) -> Option<std::ffi::OsString>,
        ) -> io::Result<Self> {
            let workers = get(WORKERS)
                .and_then(|v| v.into_string().ok())
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|v| (1..=8).contains(v))
                .unwrap_or_else(|| thread::available_parallelism().map_or(1, |n| n.get().min(4)));
            let cache_setting = get(CACHE_EXPERIMENT)
                .map(|v| {
                    v.into_string().map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("{CACHE_EXPERIMENT} must be UTF-8"),
                        )
                    })
                })
                .transpose()?;
            Ok(Self {
                workers,
                root_data_cache: root_data_cache_enabled("/dev/root", cache_setting.as_deref())?,
                guest_cache: guest_cache_enabled(get(GUEST_CACHE).as_deref())?,
                host_writeback: host_writeback_enabled(get(HOST_WRITEBACK).as_deref())?,
                host_writeback_config: host_writeback_config(
                    get(HOST_WRITEBACK_CONFIG).as_deref(),
                )?,
                directory_cache_capacity: directory_cache_capacity(
                    get(DIRECTORY_CACHE_CAPACITY).as_deref(),
                )?,
                exclusive_upper_absence_cache: exclusive_upper_absence_cache_enabled(
                    get(EXCLUSIVE_UPPER_ABSENCE_CACHE).as_deref(),
                )?,
                readonly_flush_elision: readonly_flush_elision_enabled(
                    get(READONLY_FLUSH_ELISION).as_deref(),
                )?,
                lower_data_cache: lower_data_cache_enabled(get(LOWER_DATA_CACHE).as_deref())?,
                negative_entry_cache: negative_entry_cache_enabled(
                    get(NEGATIVE_ENTRY_CACHE).as_deref(),
                )?,
                directory_names_cache: directory_names_cache_enabled(
                    get(DIRECTORY_NAMES_CACHE).as_deref(),
                )?,
                prepared_copy_up: prepared_copy_up_enabled(get(PREPARED_COPY_UP).as_deref())?,
                object_read_barriers: object_read_barriers_enabled(
                    get(OBJECT_READ_BARRIERS).as_deref(),
                )?,
                logical_directory_cache: logical_directory_cache_enabled(
                    get(LOGICAL_DIRECTORY_CACHE).as_deref(),
                )?,
                scoped_names: scoped_names_enabled(get(SCOPED_NAMES).as_deref())?,
                logical_data_cache: logical_data_cache_enabled(get(LOGICAL_DATA_CACHE).as_deref())?,
                // Preserve env::var(...).ok(): non-Unicode is unset, all other
                // strings (including empty/invalid) remain the explicit third state.
                writeback: get(WRITEBACK).and_then(|v| v.into_string().ok()),
                shared_open: shared_open_qualification_enabled(
                    get(SHARED_OPEN_QUALIFICATION).as_deref(),
                )
                .map(|enabled| enabled && cfg!(target_os = "linux")),
            })
        }
    }

    pub(crate) fn root_data_cache_enabled(tag: &str, setting: Option<&str>) -> io::Result<bool> {
        match setting {
            None | Some("root-data") => Ok(tag == "/dev/root"),
            Some("off") => Ok(false),
            Some(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{CACHE_EXPERIMENT} supports root-data or off; metadata and no-open modes are not integrated"
                ),
            )),
        }
    }

    fn boolean(setting: Option<&std::ffi::OsStr>, name: &str, default: bool) -> io::Result<bool> {
        match setting {
            None => Ok(default),
            Some(value) if value == "0" => Ok(false),
            Some(value) if value == "1" => Ok(true),
            Some(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} must be 0 or 1"),
            )),
        }
    }

    macro_rules! boolean_parser {
        ($function:ident, $name:ident, $default:expr) => {
            pub(crate) fn $function(setting: Option<&std::ffi::OsStr>) -> io::Result<bool> {
                boolean(setting, $name, $default)
            }
        };
    }
    boolean_parser!(guest_cache_enabled, GUEST_CACHE, true);
    boolean_parser!(host_writeback_enabled, HOST_WRITEBACK, false);
    boolean_parser!(
        exclusive_upper_absence_cache_enabled,
        EXCLUSIVE_UPPER_ABSENCE_CACHE,
        false
    );
    boolean_parser!(
        readonly_flush_elision_enabled,
        READONLY_FLUSH_ELISION,
        false
    );
    boolean_parser!(lower_data_cache_enabled, LOWER_DATA_CACHE, false);
    boolean_parser!(negative_entry_cache_enabled, NEGATIVE_ENTRY_CACHE, false);
    boolean_parser!(directory_names_cache_enabled, DIRECTORY_NAMES_CACHE, false);
    boolean_parser!(prepared_copy_up_enabled, PREPARED_COPY_UP, false);
    boolean_parser!(object_read_barriers_enabled, OBJECT_READ_BARRIERS, false);
    boolean_parser!(
        logical_directory_cache_enabled,
        LOGICAL_DIRECTORY_CACHE,
        false
    );
    boolean_parser!(scoped_names_enabled, SCOPED_NAMES, false);
    boolean_parser!(logical_data_cache_enabled, LOGICAL_DATA_CACHE, false);
    boolean_parser!(
        shared_open_qualification_enabled,
        SHARED_OPEN_QUALIFICATION,
        false
    );

    /// Private engineering values; never serialized into snapshots.
    #[derive(Debug, serde::Deserialize)]
    #[serde(default, deny_unknown_fields)]
    pub(crate) struct HostWritebackConfig {
        capacity_mib: usize,
        jobs: usize,
        batch_kib: usize,
        chunks: usize,
        delay_us: u64,
        pub(crate) qualification_lock: bool,
        pub(crate) synchronous: bool,
    }

    impl Default for HostWritebackConfig {
        fn default() -> Self {
            Self {
                capacity_mib: 64,
                jobs: 1024,
                batch_kib: 1024,
                chunks: 64,
                delay_us: 10_000,
                qualification_lock: false,
                synchronous: false,
            }
        }
    }

    impl HostWritebackConfig {
        pub(crate) fn options(&self) -> io::Result<WriteBufferOptions> {
            let invalid =
                || io::Error::new(io::ErrorKind::InvalidInput, "host writeback size overflow");
            let options = WriteBufferOptions {
                budget_bytes: self
                    .capacity_mib
                    .checked_mul(1024 * 1024)
                    .ok_or_else(invalid)?,
                max_jobs: self.jobs,
                batch_bytes: self.batch_kib.checked_mul(1024).ok_or_else(invalid)?,
                max_chunks: self.chunks,
                max_delay: std::time::Duration::from_micros(self.delay_us),
                object_barriers: false,
            };
            options.validate()?;
            Ok(options)
        }
    }

    pub(crate) fn host_writeback_config(
        setting: Option<&std::ffi::OsStr>,
    ) -> io::Result<HostWritebackConfig> {
        let config = match setting {
            None => HostWritebackConfig::default(),
            Some(value) => {
                let invalid = |error| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("{HOST_WRITEBACK_CONFIG}: {error}"),
                    )
                };
                let parsed: serde_json::Value =
                    serde_json::from_slice(value.as_bytes()).map_err(invalid)?;
                if !parsed.is_object() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("{HOST_WRITEBACK_CONFIG} must be a JSON object"),
                    ));
                }
                // Decode original bytes so duplicate fields are rejected.
                serde_json::from_slice(value.as_bytes()).map_err(invalid)?
            }
        };
        config.options()?;
        Ok(config)
    }

    pub(crate) fn directory_cache_capacity(setting: Option<&std::ffi::OsStr>) -> io::Result<usize> {
        let Some(setting) = setting else {
            return Ok(DEFAULT_DIRECTORY_CACHE_CAPACITY);
        };
        setting
            .to_str()
            .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|capacity| (1..=MAX_DIRECTORY_CACHE_CAPACITY).contains(capacity))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "{DIRECTORY_CACHE_CAPACITY} must be an ASCII decimal integer from 1 to 4096"
                    ),
                )
            })
    }
}
