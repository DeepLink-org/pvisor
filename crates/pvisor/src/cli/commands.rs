//! OCI cache companion frontend.
use clap::Parser;

pub fn cache_main() -> anyhow::Result<()> {
    #[derive(Parser)]
    #[command(
        name = "pvisor-cache",
        version,
        about = "Prepare or query OCI caches backed by a server, filesystem, or S3"
    )]
    struct CacheCli {
        #[command(flatten)]
        args: crate::image::cache::CacheArgs,
    }
    crate::image::cache::run(CacheCli::parse().args)
}
