use crate::api::{Shim, ShimRuntime};

impl ShimRuntime for Shim {
    fn run() -> anyhow::Result<()> {
        #[cfg(target_os = "linux")]
        {
            if crate::child::run_internal_if_requested()? {
                return Ok(());
            }
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            runtime.block_on(crate::service::bootstrap())?;
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "containerd-shim-pvisor-v2 only supports Linux hosts",
            )
            .into())
        }
    }
}
