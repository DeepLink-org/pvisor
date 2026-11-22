#[cfg(not(feature = "aws-nitro"))]
use crate::devices::virtio::fs::passthrough::PermissionSemantics;
#[cfg(not(feature = "aws-nitro"))]
use crate::devices::virtio::fs::virtual_entry::VirtualDirEntry;
#[cfg(not(feature = "aws-nitro"))]
use crate::devices::virtio::fs::OverlayConfig;

#[derive(Clone)]
pub(crate) enum OverlaySource {
    Config(Box<OverlayConfig>),
    Deferred(DeferredOverlay),
}

impl std::fmt::Debug for OverlaySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(config) => config.fmt(f),
            Self::Deferred(_) => f.write_str("DeferredOverlay"),
        }
    }
}

pub(crate) type PrepareOverlay =
    Box<dyn FnOnce() -> std::io::Result<(OverlayConfig, Vec<u8>)> + Send>;
pub(crate) type DeferredOverlay = std::sync::Arc<std::sync::Mutex<Option<PrepareOverlay>>>;

#[derive(Clone, Debug)]
pub struct FsDeviceConfig {
    pub fs_id: String,
    /// Host directory to pass through. None means a virtual-only filesystem
    /// (NullFs + AugmentFs, no host directory).
    pub shared_dir: Option<String>,
    pub semantics: PermissionSemantics,
    pub shm_size: Option<usize>,
    pub read_only: bool,
    pub managed: bool,
    #[cfg(not(feature = "aws-nitro"))]
    pub(crate) overlay: Option<OverlaySource>,
    #[cfg(not(feature = "aws-nitro"))]
    pub virtual_entries: Vec<VirtualDirEntry>,
}
