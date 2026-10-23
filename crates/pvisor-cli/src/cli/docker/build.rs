use super::*;

#[derive(Debug, Args)]
pub(crate) struct Build {
    /// Local directory used as the build context.
    context: PathBuf,
    /// Dockerfile (default: PATH/Dockerfile).
    #[arg(short = 'f', long = "file")]
    file: Option<PathBuf>,
    /// Name and optionally tag the resulting image; repeatable.
    #[arg(short = 't', long = "tag")]
    tags: Vec<String>,
    /// Explicit build-time variable; NAME forwards only that host variable.
    #[arg(long = "build-arg")]
    build_args: Vec<String>,
    /// Publish the specified stage.
    #[arg(long)]
    target: Option<String>,
    /// Native platform assertion; does not enable CPU emulation.
    #[arg(long)]
    platform: Option<String>,
    /// Always attempt to pull registry base images.
    #[arg(long)]
    pull: bool,
    /// Accepted for Docker compatibility; this builder always executes every step.
    #[arg(long)]
    no_cache: bool,
    /// Only print the resulting image ID.
    #[arg(short = 'q', long)]
    quiet: bool,
    /// Export an OCI image-layout tar: type=oci,dest=PATH.
    #[arg(short = 'o', long)]
    output: Option<String>,
    /// Write the resulting configuration digest to this file.
    #[arg(long)]
    iidfile: Option<PathBuf>,
    /// RUN networking; default uses public IPv4 TCP/DNS, none omits the NIC.
    #[arg(long,default_value="default",value_parser=["default","none"])]
    network: String,
    /// Number of vCPUs per RUN VM.
    #[arg(long, default_value_t = 1)]
    cpus: u8,
    /// Total RAM per RUN VM, not a container cgroup limit.
    #[arg(short='m',long,default_value="512m",value_parser=memory)]
    memory: u32,
    /// Maximum seconds per RUN including VM startup.
    #[arg(long, default_value_t = 600)]
    timeout: u64,
    /// Plain step/log rendering; auto currently selects the same renderer.
    #[arg(long,default_value="auto",value_parser=["auto","plain"])]
    progress: String,
}

pub(super) fn run(args: Build) -> anyhow::Result<i32> {
    let (build_args, _) = environment(&args.build_args, &[])?;
    let output = args
        .output
        .map(|value| -> anyhow::Result<PathBuf> {
            let mut fields = BTreeMap::new();
            for item in value.split(',') {
                let (k, v) = item
                    .split_once('=')
                    .context("output requires type=oci,dest=PATH")?;
                ensure!(fields.insert(k, v).is_none(), "duplicate output option");
            }
            ensure!(
                fields.len() == 2 && fields.get("type") == Some(&"oci"),
                "only type=oci,dest=PATH output is supported"
            );
            Ok(fields
                .get("dest")
                .filter(|v| !v.is_empty())
                .context("missing output destination")?
                .into())
        })
        .transpose()?;
    let options = pvisor::docker::build::BuildOptions {
        file: args.file.unwrap_or_else(|| args.context.join("Dockerfile")),
        context: args.context,
        tags: args.tags,
        build_args,
        target: args.target,
        platform: args.platform,
        pull: args.pull,
        cpus: args.cpus,
        memory_mib: args.memory,
        network: args.network != "none",
        timeout: Duration::from_secs(args.timeout),
        quiet: args.quiet,
        output,
    };
    let image = pvisor::docker::build::build(&Store::discover()?, &options)?;
    if let Some(path) = args.iidfile {
        std::fs::write(path, format!("{}\n", image.id))?;
    }
    if args.quiet {
        println!("{}", image.id);
    } else {
        println!("Successfully built {}", image.id);
        for tag in options.tags {
            println!("Successfully tagged {tag}");
        }
    }
    Ok(0)
}
