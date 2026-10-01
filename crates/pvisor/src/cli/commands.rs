//! OCI cache companion frontend.
use clap::Parser;

pub fn cache_main() -> anyhow::Result<()> {
    #[derive(Parser)]
    #[command(
        name = "pvisor-cache",
        version,
        about = "Serve or query the shared OCI file cache"
    )]
    struct CacheCli {
        #[command(flatten)]
        args: crate::image::cache::CacheArgs,
    }
    crate::image::cache::run(CacheCli::parse().args)
}
