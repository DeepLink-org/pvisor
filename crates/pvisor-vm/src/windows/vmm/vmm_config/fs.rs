#[cfg(not(any(feature = "tee", feature = "aws-nitro")))]
use std::sync::Arc;

#[cfg(not(any(feature = "tee", feature = "aws-nitro")))]
use crate::devices::virtio::fs::DynFileSystem;

//--------------------------------------------------------------------------------------------------
// Types
//--------------------------------------------------------------------------------------------------

#[derive(Clone)]
pub struct FsDeviceConfig {
    pub fs_id: String,
    pub shared_dir: String,
    pub shm_size: Option<usize>,
    pub passthrough: crate::devices::virtio::fs::passthrough::Config,
}

#[cfg(not(any(feature = "tee", feature = "aws-nitro")))]
pub struct CustomFsDeviceConfig {
    pub fs_id: String,
    pub backend: Arc<dyn DynFileSystem>,
    pub shm_size: Option<usize>,
}
