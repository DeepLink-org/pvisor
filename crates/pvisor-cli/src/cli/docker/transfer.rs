//! Image transfers for ordinary images and prepared templates.
use super::*;
use pvisor_daemon::api::{PreparedCatalog, PreparedCatalogService};

#[derive(Debug, Args)]
pub(in crate::cli) struct Push {
    image: String,
    /// Publish to s3://BUCKET/PREFIX, gs://BUCKET/PREFIX or file:///ABSOLUTE/PATH.
    #[arg(long)]
    to: Option<String>,
    /// Select an existing prepared template instead of an ordinary local image.
    #[arg(long)]
    images_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(in crate::cli) struct Pull {
    pub(super) image: String,
    /// Select the OCI platform; lazy imports retain the publication's platform.
    #[arg(long, value_parser = ["linux/amd64", "linux/arm64", "linux/arm64/v8"], conflicts_with_all = ["from", "images_dir"])]
    pub(super) platform: Option<String>,
    /// OCI registry username for authenticated pulls.
    #[arg(long, conflicts_with_all = ["from", "images_dir"])]
    pub(super) username: Option<String>,
    /// OCI registry password. Prefer --password-stdin to keep it out of argv.
    #[arg(long, requires = "username")]
    password: Option<String>,
    /// Read the OCI registry password from stdin.
    #[arg(long, requires = "username", conflicts_with = "password")]
    pub(super) password_stdin: bool,
    /// Import a fixed file:///, s3://, gs:// or HTTP(S) publication instead of pulling OCI.
    #[arg(long)]
    from: Option<String>,
    /// Allow an explicit lazy import to replace an existing local name.
    #[arg(long)]
    refresh: bool,
    /// Import into the prepared-template catalog; without --from use its registries.
    #[arg(long)]
    images_dir: Option<PathBuf>,
}

pub(super) fn push(args: Push) -> anyhow::Result<i32> {
    let destination = args
        .to
        .context("OCI registry push is not implemented; specify a repository URL with --to")?;
    Store::validate_image_destination(&destination)?;
    let receipt = if let Some(images) = args.images_dir {
        PreparedCatalog::new(images).push(&args.image, &destination)?
    } else {
        Store::discover()?.push_image(&args.image, &destination)?
    };
    println!("{}", serde_json::to_string(&receipt)?);
    Ok(0)
}

pub(super) fn pull(args: Pull) -> anyhow::Result<i32> {
    ensure!(
        !args.refresh || args.from.is_some() || args.images_dir.is_some(),
        "--refresh requires --from or --images-dir"
    );
    if let Some(source) = &args.from {
        Store::validate_image_source(source)?;
    }
    if let Some(images) = args.images_dir {
        let image =
            PreparedCatalog::new(images).pull(&args.image, args.from.as_deref(), args.refresh)?;
        println!(
            "{}",
            serde_json::json!({
                "image": args.image, "source": image.cache.repository, "mirrors": image.cache.mirrors,
            })
        );
        return Ok(0);
    }
    let mut store = Store::discover()?;
    let image = if let Some(source) = args.from {
        store.pull_image_repository(&args.image, &source, args.refresh)?
    } else {
        if let Some(username) = args.username {
            let password = if args.password_stdin {
                let mut secret = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut secret)
                    .context("read registry password from stdin")?;
                secret.trim_end_matches(['\r', '\n']).to_string()
            } else {
                args.password
                    .context("registry --username needs --password or --password-stdin")?
            };
            store.set_registry_credentials(pvisor::docker::RegistryCredentials {
                username,
                password,
            });
        }
        let mut display = progress::PullDisplay::new(&args.image);
        let mut progress = |event| display.update(event);
        match args.platform {
            Some(platform) => {
                store.pull_for_platform_with_progress(&args.image, &platform, &mut progress)?
            }
            None => store.pull_with_progress(&args.image, &mut progress)?,
        }
    };
    println!("Digest: {}\n{}", image.digest, image.reference);
    Ok(0)
}
