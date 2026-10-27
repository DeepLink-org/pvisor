//! Sequential Dockerfile builds. Each RUN self-executes a fresh native VM;
//! only its private overlay is writable. No Docker daemon or host shell is used.
#![deny(missing_docs)]
use super::*;
use pvisor_journal::api::{DurableFiles, Persistence};
use pvisor_overlay_core::OverlayCore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, os::unix::fs::PermissionsExt};
mod dockerfile;
mod filesystem;
use dockerfile::{expand, expanded_words, words};

/// Inputs for a local Dockerfile build. Unsupported syntax fails explicitly.
#[derive(Clone, Debug)]
pub struct BuildOptions {
    /// Existing local context directory; captured privately without following links.
    pub context: PathBuf,
    /// Dockerfile path; relative paths are relative to the invoking directory.
    pub file: PathBuf,
    /// Local tags published after build steps complete; each V2 HEAD commits
    /// independently. The catalog changes only after all publications succeed.
    pub tags: Vec<String>,
    /// Explicit build arguments. Unspecified host environment is never forwarded.
    pub build_args: BTreeMap<String, String>,
    /// Optional completed stage name to publish instead of the final stage.
    pub target: Option<String>,
    /// Optional native platform assertion; CPU emulation is not implemented.
    pub platform: Option<String>,
    /// Refresh registry bases even if a matching local reference exists.
    pub pull: bool,
    /// Per-RUN VM vCPU count; must be nonzero.
    pub cpus: u8,
    /// Per-RUN total guest RAM in MiB; at least 16.
    pub memory_mib: u32,
    /// Enable the existing public IPv4 TCP/DNS profile; false omits the NIC.
    pub network: bool,
    /// Upper bound for each RUN, including VM startup and teardown.
    pub timeout: Duration,
    /// Suppress RUN output and build progress; callers can print only the image ID.
    pub quiet: bool,
    /// Optional new OCI image-layout tar file. Existing files are never overwritten.
    pub output: Option<PathBuf>,
}

/// Build and register a native image using private overlays and fresh RUN VMs.
///
/// All writers exit and the VMM is reaped before COPY, export or publication.
/// Failure keeps prior tags unchanged; successful registry pulls may be retained.
/// Scratch and multi-stage builds, common configuration instructions and local
/// COPY are supported. Builds currently run without instruction-cache reuse and
/// export one flattened filesystem layer. Non-local contexts, ADD, ONBUILD,
/// RUN mounts/heredocs and foreign execution are rejected. The calling binary
/// must dispatch [`super::internal_if_requested`] before ordinary startup.
pub fn build(store: &Store, options: &BuildOptions) -> anyhow::Result<Image> {
    ensure!(
        options.cpus > 0 && options.memory_mib >= 16 && !options.timeout.is_zero(),
        "invalid build resource/time budget"
    );
    if let Some(platform) = &options.platform {
        require_native_platform(platform)?;
    }
    for tag in &options.tags {
        validate_tag(tag)?;
    }
    for key in options.build_args.keys() {
        ensure!(
            dockerfile::valid_variable(key),
            "invalid build argument {key}"
        );
    }
    ensure!(
        options.build_args.values().all(|v| !v.contains('\0')),
        "build argument contains NUL"
    );
    let context = options
        .context
        .canonicalize()
        .context("build context must be an existing local directory")?;
    ensure!(context.is_dir(), "build context is not a directory");
    for authority in [
        &store.root,
        &store.images.root,
        &crate::host_transport::host_authority_root()?,
        &socket_directory()?,
    ] {
        ensure!(
            !context.starts_with(authority) && !authority.starts_with(&context),
            "build context overlaps private storage/control authority"
        );
    }
    let instructions =
        dockerfile::parse(&read_text(&options.file).context("cannot read Dockerfile")?)?;
    if let Some(target) = &options.target {
        let stages: Vec<_> = instructions.iter().filter(|i| i.name == "FROM").collect();
        ensure!(
            target.parse::<usize>().is_ok_and(|i| i < stages.len())
                || stages.iter().any(|i| {
                    words(&i.value).is_ok_and(|v| {
                        v.len() >= 3
                            && v[v.len() - 2].eq_ignore_ascii_case("AS")
                            && v.last().unwrap().eq_ignore_ascii_case(target)
                    })
                }),
            "unknown target stage {target}"
        );
    }
    let builds = store.root.join("builds");
    private_directory(&builds)?;
    let work = tempfile::Builder::new()
        .prefix("build-")
        .tempdir_in(&builds)?;
    let snapshot = work.path().join("context");
    let specific = PathBuf::from(format!("{}.dockerignore", options.file.display()));
    let ignore_file = if specific.is_file() {
        specific
    } else {
        context.join(".dockerignore")
    };
    let ignore = match read_text(&ignore_file) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    filesystem::snapshot(&context, &snapshot, &filesystem::Ignore::new(&ignore)?)?;
    let private = Store::open(work.path().join("store"))?;
    let mut stages: Vec<(String, Built)> = Vec::new();
    let mut current: Option<Stage> = None;
    let native = native_platform();
    let arch = native.strip_prefix("linux/").unwrap();
    let mut global = BTreeMap::from([
        ("TARGETPLATFORM".into(), native.clone()),
        ("BUILDPLATFORM".into(), native.clone()),
        ("TARGETARCH".into(), arch.into()),
        ("TARGETOS".into(), "linux".into()),
    ]);
    let mut args = BTreeMap::new();
    let mut selected = None;
    for instruction in instructions {
        if !options.quiet {
            eprintln!(
                "[{}] {} {}",
                instruction.line, instruction.name, instruction.value
            );
        }
        let mut execute = || -> anyhow::Result<()> {
            if instruction.name == "ARG" {
                let (key, default) = instruction
                    .value
                    .split_once('=')
                    .map_or((instruction.value.as_str(), None), |(k, v)| (k, Some(v)));
                ensure!(dockerfile::valid_variable(key), "invalid ARG name");
                let automatic = global.get(key).cloned();
                let value = options
                    .build_args
                    .get(key)
                    .cloned()
                    .or(default
                        .map(|v| {
                            expanded_words(v, if current.is_some() { &args } else { &global })
                                .map(|v| v.join(" "))
                        })
                        .transpose()?)
                    .or(automatic);
                let scope = if current.is_some() {
                    &mut args
                } else {
                    &mut global
                };
                if let Some(value) = value {
                    scope.insert(key.into(), value);
                }
                return Ok(());
            }
            if instruction.name == "FROM" {
                if let Some(stage) = current.take() {
                    let name = stage.name.clone();
                    let built = stage.finish(work.path(), args.clone())?;
                    let target = options.target.as_ref().is_some_and(|t| {
                        t.eq_ignore_ascii_case(&name)
                            || t.parse::<usize>().ok() == Some(stages.len())
                    });
                    stages.push((name, built));
                    if target {
                        selected = Some(stages.len() - 1);
                        return Ok(());
                    }
                }
                let mut from = words(&expand(&instruction.value, &global)?)?;
                if from.first().is_some_and(|v| v.starts_with("--platform=")) {
                    require_native_platform(from.remove(0).trim_start_matches("--platform="))?;
                }
                ensure!(
                    from.len() == 1 || from.len() == 3 && from[1].eq_ignore_ascii_case("AS"),
                    "expected FROM image [AS stage]"
                );
                let name = from
                    .get(2)
                    .cloned()
                    .unwrap_or_else(|| stages.len().to_string());
                ensure!(
                    !stages.iter().any(|(n, _)| n.eq_ignore_ascii_case(&name)),
                    "duplicate stage name"
                );
                let base = if let Some(index) = stage_index(&stages, &from[0]) {
                    stages[index].1.clone()
                } else if from[0] == "scratch" {
                    scratch(work.path())?
                } else {
                    base(store, &from[0], options.pull)?
                };
                args = base.args.clone();
                current = Some(Stage::new(private.clone(), base, name, options)?);
                return Ok(());
            }
            let stage = current.as_mut().context("only ARG may precede FROM")?;
            let mut env = args.clone();
            env.extend(stage.image.env.clone());
            match instruction.name.as_str() {
                "RUN" => {
                    let argv = if instruction.value.starts_with('[') {
                        words(&instruction.value)?
                    } else {
                        let mut a = stage.shell()?;
                        a.push(instruction.value.clone());
                        a
                    };
                    stage.run(argv, &env, options)?;
                }
                "COPY" => stage.copy(&instruction.value, &env, &snapshot, &stages)?,
                "ENV" => {
                    for (k, v) in dockerfile::assignments(&instruction.value, &env)? {
                        stage.image.env.insert(k, v);
                    }
                    stage.update_env();
                }
                "LABEL" => {
                    for word in expanded_words(&instruction.value, &env)? {
                        let (k, v) = word.split_once('=').context("LABEL requires key=value")?;
                        stage.config["Labels"][k] = v.into();
                    }
                }
                "WORKDIR" => {
                    let value = expanded_words(&instruction.value, &env)?.join(" ");
                    let path = filesystem::path(&value, &stage.image.cwd)?;
                    filesystem::mkdir(&stage.core()?, &path)?;
                    stage.image.cwd = Path::new("/").join(path);
                    stage.config["WorkingDir"] =
                        stage.image.cwd.to_string_lossy().into_owned().into();
                }
                "USER" => {
                    stage.image.user = expanded_words(&instruction.value, &env)?.join(" ");
                    stage.config["User"] = stage.image.user.clone().into();
                }
                "CMD" | "ENTRYPOINT" | "SHELL" => {
                    let json_form = instruction.value.starts_with('[');
                    ensure!(
                        instruction.name != "SHELL" || json_form,
                        "SHELL requires a JSON string array"
                    );
                    let argv = if json_form {
                        words(&instruction.value)?
                    } else {
                        let mut a = stage.shell()?;
                        a.push(instruction.value.clone());
                        a
                    };
                    if instruction.name == "CMD" {
                        stage.cmd_set = true;
                        stage.image.cmd = argv.clone();
                        stage.config["Cmd"] = json!(argv);
                    } else if instruction.name == "ENTRYPOINT" {
                        stage.image.entrypoint = argv.clone();
                        stage.config["Entrypoint"] = json!(argv);
                        if !stage.cmd_set {
                            stage.image.cmd.clear();
                            stage.config["Cmd"] = json!([]);
                        }
                    } else {
                        ensure!(!argv.is_empty(), "SHELL cannot be empty");
                        stage.config["Shell"] = json!(argv);
                    }
                }
                "EXPOSE" => {
                    for port in words(&expand(&instruction.value, &env)?)? {
                        let (number, proto) = port.split_once('/').unwrap_or((&port, "tcp"));
                        ensure!(
                            number.parse::<u16>().is_ok_and(|p| p > 0)
                                && matches!(proto, "tcp" | "udp"),
                            "invalid EXPOSE port"
                        );
                        stage.config["ExposedPorts"][format!("{number}/{proto}")] = json!({});
                    }
                }
                "VOLUME" => {
                    for path in expanded_words(&instruction.value, &env)? {
                        ensure!(
                            Path::new(&path).is_absolute(),
                            "VOLUME requires absolute paths"
                        );
                        stage.config["Volumes"][&path] = json!({});
                        stage.image.volumes.push(path.into());
                    }
                }
                "STOPSIGNAL" => {
                    let value = expand(&instruction.value, &env)?;
                    stage.image.stop_signal = signal(&value)?;
                    stage.config["StopSignal"] = value.into();
                }
                _ => unreachable!(),
            }
            Ok(())
        };
        execute().with_context(|| {
            format!(
                "Dockerfile line {} ({})",
                instruction.line, instruction.name
            )
        })?;
        if selected.is_some() {
            break;
        }
    }
    if let Some(stage) = current.take() {
        let name = stage.name.clone();
        stages.push((name, stage.finish(work.path(), args.clone())?));
    }
    let index = if let Some(target) = &options.target {
        stage_index(&stages, target).context("unknown target stage")?
    } else {
        stages.len() - 1
    };
    let result = &stages[index].1;
    if let Some(output) = &options.output {
        export_layout(&result.layout, output)?;
    }
    publish(store, result, &options.tags)
}

fn read_text(path: &Path) -> io::Result<String> {
    const LIMIT: u64 = 16 * 1024 * 1024;
    let input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    let metadata = input.metadata()?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "build definition must be a regular file of at most 16 MiB",
        ));
    }
    let mut bytes = Vec::new();
    input.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "build definition grew beyond 16 MiB",
        ));
    }
    String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn stage_index(stages: &[(String, Built)], name: &str) -> Option<usize> {
    name.parse::<usize>()
        .ok()
        .filter(|i| *i < stages.len())
        .or_else(|| {
            stages
                .iter()
                .position(|(n, _)| n.eq_ignore_ascii_case(name))
        })
}
fn validate_tag(tag: &str) -> anyhow::Result<()> {
    let (name, suffix) = tag
        .rsplit_once(':')
        .filter(|(_, t)| !t.contains('/'))
        .map_or((tag, None), |(n, t)| (n, Some(t)));
    ensure!(
        !name.is_empty() && name.len() <= 255 && !name.contains('@'),
        "invalid build tag {tag}"
    );
    if let Some(suffix) = suffix {
        ensure!(
            !suffix.is_empty()
                && suffix.len() <= 128
                && !suffix.starts_with(['.', '-'])
                && suffix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
            "invalid build tag {tag}"
        );
    }
    for (index, part) in name.split('/').enumerate() {
        ensure!(
            !part.is_empty()
                && part.as_bytes()[0].is_ascii_alphanumeric()
                && part.as_bytes().last().unwrap().is_ascii_alphanumeric()
                && part.bytes().all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || b"._-".contains(&b)
                    || index == 0 && b == b':'),
            "invalid build tag {tag}"
        );
    }
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum BuildSource {
    Temporary { root: PathBuf },
    Pooled { cache: CachedImage },
}
impl BuildSource {
    fn temporary_root(&self) -> anyhow::Result<&Path> {
        match self {
            Self::Temporary { root } => Ok(root),
            Self::Pooled { .. } => bail!("build output is not a temporary working tree"),
        }
    }
}

type BuildImage = Image<BuildSource>;

fn open_build_image(store: &Store, image: &BuildImage) -> anyhow::Result<ImageRoot> {
    match &image.cache {
        BuildSource::Pooled { cache } => store.open_cached(cache, &image.digest, &image.platform),
        BuildSource::Temporary { root } => {
            let root = root.canonicalize()?;
            let work = store
                .root
                .parent()
                .context("build store has no workspace")?;
            ensure!(
                root.starts_with(work) && root != work,
                "build lower escapes its workspace"
            );
            ensure!(
                fs::symlink_metadata(&root)?.is_dir(),
                "build lower is not a directory"
            );
            Ok(ImageRoot { root, _owner: None })
        }
    }
}

pub(super) fn run_internal(store: &Store, id: &str, generation: &str) -> anyhow::Result<()> {
    ensure!(
        id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid build instance ID"
    );
    let instance: Instance<BuildSource> =
        serde_json::from_reader(File::open(store.directory(id).join("instance.json"))?)?;
    ensure!(
        instance.schema == 1 && instance.id == id && instance.generation == generation,
        "stale build instance"
    );
    let lower = open_build_image(store, &instance.image)?;
    super::run_vm(store, instance, lower)
}

#[derive(Clone)]
struct Built {
    image: BuildImage,
    config: Value,
    layout: PathBuf,
    args: BTreeMap<String, String>,
}
fn scratch(work: &Path) -> anyhow::Result<Built> {
    let root = work.join(format!("scratch-{}", uuid()));
    fs::create_dir(&root)?;
    filesystem::set_identity(&root, 0, 0, 0o755)?;
    Ok(Built {
        image: Image {
            reference: "scratch".into(),
            id: String::new(),
            digest: String::new(),
            platform: native_platform(),
            cache: BuildSource::Temporary { root },
            env: BTreeMap::new(),
            entrypoint: vec![],
            cmd: vec![],
            cwd: "/".into(),
            user: String::new(),
            stop_signal: 15,
            volumes: vec![],
            created_ms: 0,
        },
        config: json!({}),
        layout: PathBuf::new(),
        args: BTreeMap::new(),
    })
}
fn base(store: &Store, reference: &str, pull: bool) -> anyhow::Result<Built> {
    let image = if !pull && let Some(image) = store.local_image(reference)? {
        image
    } else {
        store.pull(reference)?
    };
    require_native_platform(&image.platform)?;
    let config_path = store.image_config_path(&image)?;
    let bytes = fs::read(config_path)?;
    ensure!(
        digest(&bytes) == image.id,
        "pooled image config digest mismatch"
    );
    let document = serde_json::from_slice::<Value>(&bytes)?;
    let mut config = document["config"].clone();
    if !config.is_object() {
        config = json!({});
    }
    ensure!(
        config["OnBuild"].as_array().is_none_or(Vec::is_empty),
        "base image ONBUILD triggers are not supported"
    );
    Ok(Built {
        image: image.map_cache(|cache| BuildSource::Pooled { cache }),
        config,
        layout: PathBuf::new(),
        args: BTreeMap::new(),
    })
}

struct Stage {
    store: Store,
    instance: Instance<BuildSource>,
    image: BuildImage,
    config: Value,
    name: String,
    cmd_set: bool,
    lower: ImageRoot,
}
impl Stage {
    fn new(
        store: Store,
        base: Built,
        name: String,
        options: &BuildOptions,
    ) -> anyhow::Result<Self> {
        let image_root = open_build_image(&store, &base.image)?;
        let id = format!("{}{}", uuid(), uuid());
        let directory = store.directory(&id);
        private_directory(&directory)?;
        for name in ["upper", "work"] {
            private_directory(&directory.join(name))?;
        }
        let config = Create {
            name: None,
            image: base.image.reference.clone(),
            platform: None,
            command: vec![],
            entrypoint: Some(String::new()),
            env: BTreeMap::new(),
            unset_env: vec![],
            cwd: None,
            user: None,
            mounts: vec![],
            ports: vec![],
            cpus: options.cpus,
            memory_mib: options.memory_mib,
            memory_pool: None,
            shared_kernel_base: None,
            auto_remove: false,
            interactive: false,
            tty: None,
            network: options.network,
            peers: None,
            network_name: None,
            ipv4_address: None,
        };
        let instance = Instance {
            schema: 1,
            id,
            name: format!("build_{}", uuid()),
            image: base.image.clone(),
            config,
            argv: vec![],
            env: BTreeMap::new(),
            cwd: base.image.cwd.clone(),
            user: base.image.user.clone(),
            state: "created".into(),
            generation: uuid(),
            started_ms: 0,
            exit_code: None,
            created_ms: crate::unix_now_ms(),
        };
        store.save(&instance)?;
        let stage = Self {
            store,
            instance,
            image: base.image,
            config: base.config,
            name,
            cmd_set: false,
            lower: image_root,
        };
        stage.core()?;
        let upper = stage.store.directory(&stage.instance.id).join("upper");
        fs::set_permissions(&upper, fs::Permissions::from_mode(0o700))?;
        // Enable virtual COPY ownership in RUN even when no resolver layer is attached.
        let root = stage.lower.rootfs();
        let (uid, gid, mode) = filesystem::identity(root, &fs::symlink_metadata(root)?)?;
        pvisor_overlay_core::sys::set_xattr(
            &upper,
            std::ffi::OsStr::new("user.containers.override_stat"),
            format!("{uid}:{gid}:0{mode:o}").as_bytes(),
            0,
        )?;
        Ok(stage)
    }
    fn core(&self) -> anyhow::Result<OverlayCore> {
        let d = self.store.directory(&self.instance.id);
        Ok(OverlayCore::new(
            vec![self.lower.rootfs().to_owned()],
            d.join("upper"),
            Some(d.join("work")),
        )?)
    }
    fn shell(&self) -> anyhow::Result<Vec<String>> {
        if self.config["Shell"].is_array() {
            let shell: Vec<String> = serde_json::from_value(self.config["Shell"].clone())?;
            ensure!(!shell.is_empty(), "invalid base image shell");
            Ok(shell)
        } else {
            Ok(vec!["/bin/sh".into(), "-c".into()])
        }
    }
    fn update_env(&mut self) {
        self.config["Env"] = json!(
            self.image
                .env
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
        );
    }
    fn run(
        &mut self,
        argv: Vec<String>,
        env: &BTreeMap<String, String>,
        options: &BuildOptions,
    ) -> anyhow::Result<()> {
        ensure!(!argv.is_empty(), "RUN has no command");
        self.instance.argv = argv;
        self.instance.env = env.clone();
        self.instance.cwd = self.image.cwd.clone();
        self.instance.user = self.image.user.clone();
        self.instance.generation = uuid();
        self.store.save(&self.instance)?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("--pvisor-internal-docker-build-vm")
            .arg(&self.store.root)
            .arg(&self.instance.id)
            .arg(&self.instance.generation)
            .stdin(Stdio::piped());
        if options.quiet {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        } else {
            command
                .stdout(Stdio::from(io::stderr()))
                .stderr(Stdio::inherit());
        }
        // No detached owner: closing this frontend's pipe also kills the VMM on SIGKILL.
        let mut child = ChildGuard(command.spawn()?);
        let deadline = Instant::now()
            .checked_add(options.timeout)
            .context("RUN timeout is too large")?;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            ensure!(Instant::now() < deadline, "RUN exceeded its time budget");
            std::thread::sleep(Duration::from_millis(10));
        };
        ensure!(
            status.success(),
            "RUN exited with status {}",
            status.code().unwrap_or(128 + status.signal().unwrap_or(9))
        );
        Ok(())
    }
    fn copy(
        &self,
        value: &str,
        env: &BTreeMap<String, String>,
        context: &Path,
        stages: &[(String, Built)],
    ) -> anyhow::Result<()> {
        let mut rest = value;
        let mut from = None;
        let mut owner = (0, 0);
        let mut mode = None;
        while rest.starts_with("--") {
            let (flag, next) = rest
                .split_once(char::is_whitespace)
                .context("COPY has no source")?;
            let (key, val) = flag.split_once('=').context("COPY flags require =value")?;
            let val = expand(val, env)?;
            match key {
                "--from" => from = Some(val),
                "--chmod" => {
                    let parsed = u32::from_str_radix(&val, 8)?;
                    ensure!(parsed <= 0o7777, "invalid COPY mode");
                    mode = Some(parsed);
                }
                "--chown" => {
                    let (a, b) = val.split_once(':').unwrap_or((&val, &val));
                    owner = (
                        a.parse()
                            .context("COPY --chown currently requires numeric UID:GID")?,
                        b.parse()?,
                    );
                }
                _ => bail!("unsupported COPY flag {key}"),
            };
            rest = next.trim_start();
        }
        let mut paths = expanded_words(rest, env)?;
        ensure!(paths.len() >= 2, "COPY requires sources and destination");
        let destination = paths.pop().unwrap();
        let root = if let Some(from) = from {
            stages[stage_index(stages, &from).context("COPY --from must name a completed stage")?]
                .1
                .image
                .cache
                .temporary_root()?
        } else {
            context
        };
        let core = self.core()?;
        let target = filesystem::path(&destination, &self.image.cwd)?;
        let target = filesystem::resolve(&core, &target, true)?;
        let is_dir = destination.ends_with('/')
            || core
                .resolve_checked(&target)?
                .is_some_and(|e| fs::symlink_metadata(e.path).is_ok_and(|m| m.is_dir()));
        let mut selected = Vec::new();
        for source in paths {
            ensure!(
                !Path::new(&source)
                    .components()
                    .any(|c| c == std::path::Component::ParentDir),
                "COPY source escapes context"
            );
            let source = source.trim_start_matches('/').trim_end_matches('/');
            if source.contains(['*', '?', '[']) {
                let pattern = globset::GlobBuilder::new(source)
                    .literal_separator(true)
                    .build()?
                    .compile_matcher();
                fn scan(
                    root: &Path,
                    rel: &Path,
                    pattern: &globset::GlobMatcher,
                    out: &mut Vec<PathBuf>,
                ) -> anyhow::Result<()> {
                    for entry in fs::read_dir(root.join(rel))? {
                        let entry = entry?;
                        let p = rel.join(entry.file_name());
                        if pattern.is_match(&p) {
                            out.push(p);
                        } else if entry.file_type()?.is_dir() {
                            scan(root, &p, pattern, out)?;
                        }
                    }
                    Ok(())
                }
                let before = selected.len();
                scan(root, Path::new(""), &pattern, &mut selected)?;
                ensure!(
                    selected.len() > before,
                    "COPY source pattern matched no files"
                );
            } else {
                selected.push(PathBuf::from(source));
            }
        }
        ensure!(
            selected.len() == 1 || is_dir,
            "multiple COPY sources require directory destination ending in /"
        );
        let mut links = BTreeMap::new();
        for source in selected {
            let mut prefix = root.to_path_buf();
            for component in source.components() {
                prefix.push(component.as_os_str());
                if prefix != root.join(&source) {
                    ensure!(
                        fs::symlink_metadata(&prefix)?.is_dir(),
                        "COPY source traverses a symlink or non-directory"
                    );
                }
            }
            let full = root.join(&source);
            let metadata = fs::symlink_metadata(&full)
                .context("COPY source is missing or excluded by .dockerignore")?;
            let dest = if metadata.is_dir() {
                target.clone()
            } else if is_dir {
                target.join(source.file_name().context("invalid COPY source")?)
            } else {
                target.clone()
            };
            filesystem::copy(&core, &full, &dest, owner, mode, &mut links)?;
        }
        Ok(())
    }
    fn finish(mut self, work: &Path, args: BTreeMap<String, String>) -> anyhow::Result<Built> {
        let directory = work.join(format!("image-{}", uuid()));
        fs::create_dir(&directory)?;
        fs::create_dir_all(directory.join("blobs/sha256"))?;
        let layer = directory.join("layer.tar");
        filesystem::export(&self.core()?, &layer)?;
        let layer_digest = file_digest(&layer)?;
        let size = fs::metadata(&layer)?.len();
        fs::rename(
            &layer,
            directory.join("blobs/sha256").join(&layer_digest[7..]),
        )?;
        let arch = native_platform();
        let arch = arch.strip_prefix("linux/").unwrap();
        let config = json!({"architecture":arch,"os":"linux","config":self.config,"rootfs":{"type":"layers","diff_ids":[layer_digest]},"history":[{"created_by":"pvisor build (flattened VM overlay)"}]});
        let config_desc = blob(
            &directory,
            &config,
            "application/vnd.oci.image.config.v1+json",
        )?;
        let manifest = json!({"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":config_desc,"layers":[{"mediaType":"application/vnd.oci.image.layer.v1.tar","digest":layer_digest,"size":size}]});
        let manifest_desc = blob(
            &directory,
            &manifest,
            "application/vnd.oci.image.manifest.v1+json",
        )?;
        fs::write(
            directory.join("oci-layout"),
            b"{\"imageLayoutVersion\":\"1.0.0\"}",
        )?;
        fs::write(
            directory.join("index.json"),
            serde_json::to_vec(&json!({"schemaVersion":2,"manifests":[manifest_desc]}))?,
        )?;
        let rootfs = directory.join("rootfs");
        fs::create_dir(&rootfs)?;
        import_layer(
            &directory.join("blobs/sha256").join(&layer_digest[7..]),
            &rootfs,
        )?;
        self.image.id = config_desc["digest"].as_str().unwrap().into();
        self.image.digest = manifest_desc["digest"].as_str().unwrap().into();
        self.image.cache = BuildSource::Temporary { root: rootfs };
        self.image.created_ms = crate::unix_now_ms();
        Ok(Built {
            image: self.image.clone(),
            config: self.config.clone(),
            layout: directory,
            args,
        })
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex(&Sha256::digest(bytes)))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn file_digest(path: &Path) -> anyhow::Result<String> {
    let mut input = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("sha256:{}", hex(&hash.finalize())))
}
fn blob(root: &Path, value: &Value, media: &str) -> anyhow::Result<Value> {
    let bytes = serde_json::to_vec(value)?;
    let digest = digest(&bytes);
    fs::write(root.join("blobs/sha256").join(&digest[7..]), &bytes)?;
    Ok(json!({"mediaType":media,"digest":digest,"size":bytes.len()}))
}

fn import_layer(layer: &Path, root: &Path) -> anyhow::Result<()> {
    crate::image::oci::apply_layer(layer, "application/vnd.oci.image.layer.v1.tar", root)?;
    let mut directories = Vec::new();
    for entry in tar::Archive::new(File::open(layer)?).entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let path = root.join(path);
        let metadata = fs::symlink_metadata(&path)?;
        filesystem::set_identity(
            &path,
            u32::try_from(entry.header().uid()?)?,
            u32::try_from(entry.header().gid()?)?,
            entry.header().mode()?,
        )?;
        if let Some(pax) = entry.pax_extensions()? {
            for attr in pax {
                let attr = attr?;
                if let Some(key) = attr.key()?.strip_prefix("SCHILY.xattr.") {
                    pvisor_overlay_core::sys::set_xattr(
                        &path,
                        std::ffi::OsStr::new(key),
                        attr.value_bytes(),
                        0,
                    )?;
                }
            }
        }
        if !metadata.file_type().is_symlink() {
            let time = fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(entry.header().mtime()?));
            if metadata.is_dir() {
                directories.push((path, time));
            } else {
                File::open(&path)?.set_times(time)?;
            }
        }
    }
    for (path, time) in directories.into_iter().rev() {
        File::open(path)?.set_times(time)?;
    }
    Ok(())
}
fn export_layout(layout: &Path, output: &Path) -> anyhow::Result<()> {
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    let mut tar = tar::Builder::new(temporary.reopen()?);
    tar.append_path_with_name(layout.join("oci-layout"), "oci-layout")?;
    tar.append_path_with_name(layout.join("index.json"), "index.json")?;
    tar.append_dir_all("blobs", layout.join("blobs"))?;
    tar.finish()?;
    drop(tar);
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(output)?;
    Persistence::sync_directory(parent)?;
    Ok(())
}
fn publish(store: &Store, built: &Built, tags: &[String]) -> anyhow::Result<Image> {
    // Move the finished tree into the existing publisher's confined OCI source
    // layout. Only metadata and pooled contents survive this temporary owner.
    let staging = store.images.staging()?;
    let source = crate::image::oci::ImageStore::new(Some(staging.path().into()))?;
    let image = built.image.clone();
    let rootfs = staging
        .path()
        .join("rootfs-v3/sha256")
        .join(&image.digest[7..]);
    fs::rename(built.image.cache.temporary_root()?, &rootfs)?;
    let manifests = staging.path().join("metadata/manifests-v1");
    fs::create_dir_all(&manifests)?;
    fs::copy(
        built.layout.join("blobs/sha256").join(&image.digest[7..]),
        manifests.join(format!("{}.json", &image.digest[7..])),
    )?;
    let config = fs::read(built.layout.join("blobs/sha256").join(&image.id[7..]))?;
    let tags = if tags.is_empty() {
        vec![image.id.clone()]
    } else {
        tags.iter().map(|t| normalize_reference(t)).collect()
    };
    // Keep pool HEAD and catalog publication under the same locks as pull.
    // Canonical ordering also handles overlapping tag sets and alias spellings.
    let mut lock_tags = BTreeSet::new();
    let mut unique_tags = Vec::new();
    for tag in tags {
        if lock_tags.insert(image_reference_key(&tag)?) {
            unique_tags.push(tag);
        }
    }
    let architecture = canonical_platform(&image.platform)?
        .strip_prefix("linux/")
        .unwrap();
    let _pulls = lock_tags
        .iter()
        .map(|tag| store.image_pull_lock(tag, architecture))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut published = Vec::new();
    for tag in &unique_tags {
        let mut entry = image.clone();
        entry.reference = tag.clone();
        published.push(store.pool_image(entry, &rootfs, &source, &config)?);
    }
    let _guard = store.lock()?;
    let mut catalog = store.catalog()?;
    for entry in &published {
        catalog.insert(image_reference_key(&entry.reference)?, entry.clone());
    }
    store.save_catalog(&catalog)?;
    Ok(published.remove(0))
}

#[cfg(test)]
mod tests;
