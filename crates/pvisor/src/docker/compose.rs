//! A deliberately bounded Compose profile, without a Docker daemon.
use super::*;
use std::collections::BTreeSet;

mod building;
mod readiness;
mod reconcile;
#[cfg(test)]
mod tests_p0;
mod volumes;

pub use building::BuildSettings;

/// When `up` builds a service's local image.
#[derive(Clone, Copy, Debug, Default)]
pub enum BuildPolicy {
    /// Build only when the selected local image is missing.
    #[default]
    Missing,
    /// Build every selected service with a build section before convergence.
    Always,
    /// Never build; missing build-only images fail without pulling generated tags.
    Never,
}

/// How `up` reconciles existing service containers.
#[derive(Clone, Copy, Debug, Default)]
pub enum RecreatePolicy {
    /// Replace containers whose resolved configuration or local image changed.
    #[default]
    Changed,
    /// Replace all selected existing containers.
    Always,
    /// Keep existing containers even when their configuration or image changed.
    Never,
}

/// Controls detached convergence. Builds and validation precede replacement.
#[derive(Clone, Debug)]
pub struct UpOptions {
    /// Local image build policy; defaults to building missing images.
    pub build: BuildPolicy,
    /// Container replacement policy; defaults to replacing changed services.
    pub recreate: RecreatePolicy,
    /// Include dependencies in the selected services; defaults to true.
    pub dependencies: bool,
    /// Grace before force-stop when replacing a running service; defaults to 10 s.
    pub timeout: Duration,
    /// Wait for all selected services to be running/healthy after startup.
    pub wait: bool,
    /// Shared bound for dependency/readiness waits; defaults to 60 seconds.
    /// Existing VM startup/transport operations retain their own bounded waits.
    pub wait_timeout: Duration,
}

impl Default for UpOptions {
    fn default() -> Self {
        Self {
            build: BuildPolicy::Missing,
            recreate: RecreatePolicy::Changed,
            dependencies: true,
            timeout: Duration::from_secs(10),
            wait: false,
            wait_timeout: Duration::from_secs(60),
        }
    }
}

/// Durable ownership published atomically with a container record.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Membership {
    /// Validated Compose project name, scoped to this Docker store.
    pub project: String,
    /// Service key; this profile creates exactly one container per service.
    pub service: String,
    /// Hash of the resolved service configuration, excluding image tag contents.
    pub config_hash: String,
    /// Previous container ID while a validated replacement is pending.
    /// Pending containers are never started; retry/down may discard them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
    /// Effective owner-supervised check, including image defaults and overrides.
    /// None means no check (or explicitly disabled), not healthy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<health::Healthcheck>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "version", skip_serializing)]
    _version: Option<String>,
    services: BTreeMap<String, Service>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    volumes: BTreeMap<String, Option<volumes::Definition>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Service {
    #[serde(default)]
    image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    build: Option<building::Definition>,
    #[serde(default)]
    command: Option<Words>,
    #[serde(default)]
    entrypoint: Option<Words>,
    #[serde(default)]
    environment: Environment,
    #[serde(default, skip_serializing)]
    env_file: Option<Files>,
    #[serde(default)]
    volumes: Vec<String>,
    #[serde(default)]
    ports: Vec<String>,
    #[serde(default)]
    depends_on: readiness::Dependencies,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    healthcheck: Option<readiness::Definition>,
    #[serde(default)]
    working_dir: Option<PathBuf>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    container_name: Option<String>,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    cpus: Option<u8>,
    #[serde(default)]
    mem_limit: Option<String>,
    #[serde(default)]
    stdin_open: bool,
    #[serde(default)]
    tty: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum Words {
    List(Vec<String>),
    String(String),
}
impl Words {
    fn words(&self) -> anyhow::Result<Vec<String>> {
        let words = match self {
            Self::List(words) => Ok(words.clone()),
            Self::String(value) => shlex::split(value).context("unbalanced command quotes"),
        }?;
        ensure!(
            words.iter().all(|word| !word.contains('\0')),
            "command contains NUL"
        );
        Ok(words)
    }
}

impl Service {
    fn argv(&self, image: &Image) -> anyhow::Result<Vec<String>> {
        let mut argv = match &self.entrypoint {
            Some(entrypoint) => entrypoint.words()?,
            None => image.entrypoint.clone(),
        };
        match &self.command {
            Some(command) => argv.extend(command.words()?),
            // Compose discards the image CMD for every non-null entrypoint,
            // including an explicitly empty entrypoint.
            None if self.entrypoint.is_none() => argv.extend(image.cmd.clone()),
            None => {}
        }
        ensure!(
            !argv.is_empty() && !argv[0].is_empty(),
            "No command specified"
        );
        Ok(argv)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum Files {
    One(PathBuf),
    Many(Vec<PathBuf>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum Environment {
    Map(BTreeMap<String, Option<String>>),
    List(Vec<String>),
}
impl Default for Environment {
    fn default() -> Self {
        Self::Map(BTreeMap::new())
    }
}

/// An opaque, validated image-based Compose project.
///
/// Loading is read-only and never opens a Docker store. Unknown fields fail
/// closed, except `x-*` extension fields and the obsolete `version` key.
/// Operations are serialized per project; individual container/catalog locks
/// retain their existing contracts. Multi-service changes are not transactions:
/// failures retain already-created resources for inspection, retry or `down`.
pub struct Project {
    name: String,
    document: Document,
}

impl Project {
    /// Load one YAML/JSON file. With no file, search the current directory and
    /// its ancestors for compose.yaml, compose.yml, docker-compose.yaml, then
    /// docker-compose.yml. Relative binds and env_file paths use its directory.
    ///
    /// Project naming precedence is explicit name, COMPOSE_PROJECT_NAME,
    /// top-level name, then normalized directory basename. Shell variables
    /// override the project's `.env`. Interpolation supports `$VAR`, `${VAR}`,
    /// `:-`, `-`, `:?`, `?`, and `$$`; other expressions are rejected.
    /// YAML mapping merges resolve before interpolation; explicit keys win and
    /// earlier mappings in merge sequences take precedence. Custom tags fail.
    /// Null command/entrypoint inherit; empty values clear the image defaults.
    /// Image-dependent executable validation occurs during admission, not load.
    /// No shell or host command is executed. Config output can contain secrets.
    pub fn load(file: Option<&Path>, name: Option<&str>) -> anyhow::Result<Self> {
        let file = match file {
            Some(file) => file.canonicalize().context("read Compose file")?,
            None => discover_file()?,
        };
        let base = file.parent().context("Compose file has no parent")?;
        let shell: BTreeMap<String, String> = std::env::vars().collect();
        let mut variables = BTreeMap::new();
        let dotenv = base.join(".env");
        match fs::read_to_string(&dotenv) {
            Ok(text) => read_environment(&text, &mut variables, &shell)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("read Compose .env"),
        }
        variables.extend(shell.clone());
        let mut value: serde_yaml::Value = serde_yaml::from_str(&fs::read_to_string(&file)?)?;
        value.apply_merge().context("invalid Compose YAML merge")?;
        expand_value(&mut value, &variables)?;
        let mut document: Document = serde_yaml::from_value(value)
            .context("unsupported or invalid Compose configuration")?;
        let name = name
            .map(str::to_owned)
            .or_else(|| variables.get("COMPOSE_PROJECT_NAME").cloned())
            .or_else(|| document.name.clone())
            .unwrap_or_else(|| {
                base.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .chars()
                    .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_".contains(*c))
                    .collect()
            });
        ensure!(project_name(&name), "invalid Compose project name: {name}");
        ensure!(
            !document.services.is_empty(),
            "Compose services must not be empty"
        );
        document.name = Some(name.clone());
        let mut volume_names = BTreeSet::new();
        for (key, definition) in &mut document.volumes {
            let definition = definition.get_or_insert_with(Default::default);
            definition.resolve(&name, key)?;
            ensure!(
                volume_names.insert(definition.name.clone()),
                "duplicate volume name"
            );
        }
        let mut names = BTreeSet::new();
        for (key, service) in &mut document.services {
            ensure!(
                dns_name(key) && key == &key.to_ascii_lowercase(),
                "service name must be a lowercase DNS label: {key}"
            );
            ensure!(
                service
                    .image
                    .as_ref()
                    .is_some_and(|image| !image.trim().is_empty())
                    || service.image.is_none() && service.build.is_some(),
                "service {key} needs an image or build"
            );
            if let Some(build) = &mut service.build {
                build.resolve(base, &variables)?;
            }
            let container = service
                .container_name
                .get_or_insert_with(|| format!("{name}-{key}-1"));
            ensure!(
                dns_name(container),
                "container name must be a DNS label of at most 63 bytes"
            );
            ensure!(
                names.insert(container.to_ascii_lowercase()),
                "duplicate container name"
            );
            if let Some(command) = &service.command {
                let words = command.words()?;
                service.command = Some(Words::List(words));
            }
            if let Some(entrypoint) = &service.entrypoint {
                let words = entrypoint.words()?;
                service.entrypoint = Some(Words::List(words));
            }
            if let Some(platform) = &service.platform {
                canonical_platform(platform)?;
            }
            if let Some(healthcheck) = &service.healthcheck {
                healthcheck.validate()?;
            }
            ensure!(
                service.cpus.unwrap_or(2) > 0,
                "cpus must be a positive whole vCPU count"
            );
            parse_memory_limit(service.mem_limit.as_deref().unwrap_or("512m"))?;
            if let Some(cwd) = &service.working_dir {
                ensure!(cwd.is_absolute(), "working_dir must be absolute");
            }
            let mut environment = BTreeMap::new();
            let files = match service.env_file.take() {
                Some(Files::One(file)) => vec![file],
                Some(Files::Many(files)) => files,
                None => vec![],
            };
            for file in files {
                let mut values = BTreeMap::new();
                read_environment(
                    &fs::read_to_string(base.join(file))?,
                    &mut values,
                    &variables,
                )?;
                environment.extend(values.into_iter().map(|(key, value)| (key, Some(value))));
            }
            let overrides = match &service.environment {
                Environment::Map(map) => map.clone(),
                Environment::List(list) => list
                    .iter()
                    .map(|entry| match entry.split_once('=') {
                        Some((key, value)) => (key.into(), Some(value.into())),
                        None => (entry.clone(), None),
                    })
                    .collect(),
            };
            for (key, value) in overrides {
                ensure!(variable_name(&key), "invalid environment name: {key}");
                environment.insert(key.clone(), value.or_else(|| variables.get(&key).cloned()));
            }
            service.environment = Environment::Map(environment);
            if let Environment::Map(environment) = &service.environment {
                ensure!(
                    environment
                        .values()
                        .flatten()
                        .all(|value| !value.contains('\0')),
                    "environment contains NUL"
                );
            }
            for volume in &mut service.volumes {
                let parts: Vec<_> = volume.split(':').collect();
                ensure!(
                    (2..=3).contains(&parts.len()),
                    "invalid Compose mount: {volume}"
                );
                ensure!(
                    parts.len() == 2 || ["ro", "rw"].contains(&parts[2]),
                    "unsupported bind mode"
                );
                validate_target(Path::new(parts[1]))?;
                let source = if parts[0].starts_with('.') || parts[0].starts_with('/') {
                    base.join(parts[0]).to_string_lossy().into_owned()
                } else {
                    ensure!(
                        document.volumes.contains_key(parts[0]),
                        "undefined volume: {}",
                        parts[0]
                    );
                    parts[0].to_owned()
                };
                *volume = format!("{}:{}:{}", source, parts[1], parts.get(2).unwrap_or(&"rw"));
            }
            let mut targets = BTreeSet::new();
            for volume in &service.volumes {
                ensure!(
                    targets.insert(volume.split(':').nth(1)),
                    "duplicate bind target"
                );
            }
            for port in &service.ports {
                parse_port_mapping(port)?;
            }
        }
        // Full-graph validation precedes every operation, including selection.
        let project = Self { name, document };
        project.order(&[], true)?;
        Ok(project)
    }

    /// Validated project name; projects with this name share lifecycle in one store.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Render resolved configuration as YAML, including resolved environment.
    /// Literal dollars in string values are escaped as `$$`, so loading this
    /// output does not interpret already-resolved values as new substitutions.
    /// No Docker storage or VM is touched; callers must protect secret output.
    pub fn config(&self) -> anyhow::Result<String> {
        let mut value = serde_yaml::to_value(&self.document)?;
        escape_config_dollars(&mut value);
        Ok(serde_yaml::to_string(&value)?)
    }

    /// Select services in dependency order, optionally including dependencies.
    /// Empty selection means all services. Unknown services and cycles fail.
    pub fn order(&self, services: &[String], dependencies: bool) -> anyhow::Result<Vec<String>> {
        fn visit(
            project: &Project,
            name: &str,
            active: &mut BTreeSet<String>,
            done: &mut BTreeSet<String>,
            ordered: &mut Vec<String>,
        ) -> anyhow::Result<()> {
            if done.contains(name) {
                return Ok(());
            }
            let service = project
                .document
                .services
                .get(name)
                .with_context(|| format!("no such service: {name}"))?;
            ensure!(
                active.insert(name.into()),
                "dependency cycle at service {name}"
            );
            for (dependency, _) in service.depends_on.entries() {
                visit(project, &dependency, active, done, ordered)?;
            }
            active.remove(name);
            done.insert(name.into());
            ordered.push(name.into());
            Ok(())
        }
        let selected: BTreeSet<_> = if services.is_empty() {
            self.document.services.keys().cloned().collect()
        } else {
            services.iter().cloned().collect()
        };
        let mut ordered = vec![];
        let mut done = BTreeSet::new();
        for name in &selected {
            visit(self, name, &mut BTreeSet::new(), &mut done, &mut ordered)?;
        }
        if !dependencies {
            ordered.retain(|name| selected.contains(name));
        }
        Ok(ordered)
    }

    fn lease(&self, store: &Store) -> anyhow::Result<File> {
        let directory = store.root.join("compose");
        private_directory(&directory)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(directory.join(format!("{}.lock", self.name)))?;
        file.lock_exclusive()?;
        Ok(file)
    }

    /// List only recorded members of the selected services, including stopped
    /// containers. A same-named ordinary container is never a member.
    pub fn instances(&self, store: &Store, services: &[String]) -> anyhow::Result<Vec<Instance>> {
        let order = self.order(services, false)?;
        let instances = store.instances(true)?;
        let mut selected = vec![];
        for name in order {
            let mut found = false;
            for instance in &instances {
                if instance.compose.as_ref().is_some_and(|member| {
                    member.project == self.name
                        && member.service == name
                        && member.replaces.is_none()
                }) {
                    ensure!(!found, "multiple containers recorded for service {name}");
                    found = true;
                    selected.push(instance.clone());
                }
            }
        }
        Ok(selected)
    }

    /// Pull selected images and their explicit platforms, without creating VMs.
    /// Dependencies are not implicitly selected. Earlier successful pulls survive
    /// later failure; progress callbacks follow Store's pull contract.
    pub fn pull(
        &self,
        store: &Store,
        services: &[String],
        progress: &mut dyn FnMut(PullProgress),
    ) -> anyhow::Result<()> {
        for name in self.order(services, false)? {
            let service = &self.document.services[&name];
            let Some(image) = &service.image else {
                continue;
            };
            if let Some(platform) = &service.platform {
                store.pull_for_platform_with_progress(image, platform, progress)?;
            } else {
                store.pull_with_progress(image, progress)?;
            }
        }
        Ok(())
    }

    /// Create/start one detached VM per selected service and its dependencies.
    /// Existing unchanged members are reused; changed configurations/images are
    /// replaced according to options. Builds and replacement admission complete
    /// before any old container is stopped. Mounted volumes survive replacement;
    /// old container writable uppers do not. There is no rollback after removal.
    /// Dependency order means process startup, not application readiness.
    /// Failure retains earlier effects and the private project network.
    pub fn up(
        &self,
        store: &Store,
        services: &[String],
        options: &UpOptions,
        progress: &mut dyn FnMut(PullProgress),
    ) -> anyhow::Result<Vec<Instance>> {
        let _lease = self.lease(store)?;
        ensure!(
            !options.wait_timeout.is_zero(),
            "readiness timeout must be positive"
        );
        ensure!(
            Instant::now().checked_add(options.wait_timeout).is_some(),
            "readiness timeout is too large"
        );
        let instances = self.reconcile(store, services, options, progress)?;
        let deadline = Instant::now()
            .checked_add(options.wait_timeout)
            .context("readiness timeout is too large")?;
        let mut started = vec![];
        for instance in instances {
            let service = instance.compose.as_ref().unwrap().service.clone();
            if options.dependencies {
                self.wait_dependencies(store, &service, deadline)?;
            }
            let completed_job = self.is_completion_job(&service)
                && instance.state == "exited"
                && instance.exit_code == Some(0);
            if instance.state == "running" || completed_job {
                started.push(instance);
            } else {
                started.push(store.start(&instance.id)?);
            }
        }
        if options.wait {
            for instance in &started {
                self.wait_ready(store, instance, deadline)?;
            }
        }
        Ok(started)
    }

    /// Start existing selected members in dependency order without creating
    /// containers or starting unselected dependencies. Missing services fail
    /// before the first start; failures retain earlier starts. A nonzero exit
    /// already observed during start is an error, not successful startup.
    /// Running services are not monitored for subsequent exits by this call.
    pub fn start(&self, store: &Store, services: &[String]) -> anyhow::Result<Vec<Instance>> {
        let _lease = self.lease(store)?;
        let instances = self.instances(store, services)?;
        ensure!(
            instances.len() == self.order(services, false)?.len(),
            "service has no container; use compose up -d"
        );
        instances
            .into_iter()
            .map(|instance| {
                let instance = if instance.state == "running" {
                    instance
                } else {
                    store.start(&instance.id)?
                };
                ensure!(
                    instance.state != "exited" || instance.exit_code == Some(0),
                    "service {} exited during start with code {:?}; inspect compose logs",
                    instance.name,
                    instance.exit_code
                );
                Ok(instance)
            })
            .collect()
    }

    /// Stop selected recorded members in reverse dependency order, retaining
    /// containers, writable files and binds. Earlier stops survive later error.
    pub fn stop(
        &self,
        store: &Store,
        services: &[String],
        timeout: Duration,
    ) -> anyhow::Result<()> {
        let _lease = self.lease(store)?;
        for instance in self.instances(store, services)?.into_iter().rev() {
            store.stop(&instance.id, timeout, None)?;
        }
        Ok(())
    }

    /// Stop and remove project members, then remove the owned default network.
    /// Orphans (services absent from this file) require remove_orphans=true.
    /// Images and host bind sources are never deleted. Named volumes survive
    /// unless remove_volumes is true and their name is declared in this file.
    /// Historical undeclared and external volumes are never removed, including
    /// with remove_orphans. Current declarations must also match recorded ownership.
    /// Volume removal refuses references from any remaining container.
    /// Attached foreign/orphan containers block network deletion. Partial errors
    /// retain remaining resources; repeat down after reconciling the failure.
    pub fn down(
        &self,
        store: &Store,
        timeout: Duration,
        remove_orphans: bool,
        remove_volumes: bool,
    ) -> anyhow::Result<()> {
        let _lease = self.lease(store)?;
        self.cleanup_pending(store, remove_volumes)?;
        let mut instances = self.instances(store, &[])?;
        if remove_orphans {
            let ids: BTreeSet<_> = instances
                .iter()
                .map(|instance| instance.id.clone())
                .collect();
            instances.extend(store.instances(true)?.into_iter().filter(|instance| {
                !ids.contains(&instance.id)
                    && instance
                        .compose
                        .as_ref()
                        .is_some_and(|member| member.project == self.name)
            }));
        }
        for instance in instances.into_iter().rev() {
            store.stop(&instance.id, timeout, None)?;
            store.remove(&instance.id, remove_volumes)?;
        }
        if remove_volumes {
            self.remove_volumes(store)?;
        }
        if let Some(network) = store.networks()?.into_iter().find(|network| {
            network.name == format!("{}_default", self.name)
                && network.compose_project.as_deref() == Some(&self.name)
        }) {
            store.remove_network(&network.id, false)?;
        }
        Ok(())
    }

    fn create_request(&self, name: &str, store: &Store, image: &Image) -> anyhow::Result<Create> {
        let service = &self.document.services[name];
        let Environment::Map(environment) = &service.environment else {
            unreachable!()
        };
        let mut mounts = vec![];
        for volume in &service.volumes {
            let parts: Vec<_> = volume.split(':').collect();
            let (source, volume) = if parts[0].starts_with('/') {
                (
                    Path::new(parts[0])
                        .canonicalize()
                        .context("bind source must be an existing directory or ordinary file")?,
                    None,
                )
            } else {
                let name = self.volume_name(parts[0]);
                (store.root.join("volumes").join(name), Some(name.to_owned()))
            };
            ensure!(
                source.is_dir() || volume.is_none() && source.is_file(),
                "bind source must be an existing directory or ordinary file"
            );
            mounts.push(Mount {
                source,
                target: parts[1].into(),
                read_only: parts[2] == "ro",
                staged: false,
                volume,
            });
        }
        let mut config = Create {
            name: service.container_name.clone(),
            image: self.image_name(name),
            platform: service.platform.clone(),
            // Resolve against the same pinned image used for admission, then
            // use Create's existing explicit-argv path. Its empty command
            // sentinel cannot represent Compose's explicit empty CMD by itself.
            command: service.argv(image)?,
            entrypoint: Some(String::new()),
            env: environment
                .iter()
                .filter_map(|(key, value)| value.as_ref().map(|value| (key.clone(), value.clone())))
                .collect(),
            unset_env: environment
                .iter()
                .filter(|(_, value)| value.is_none())
                .map(|(key, _)| key.clone())
                .collect(),
            cwd: service.working_dir.clone(),
            user: service.user.clone(),
            mounts,
            ports: service
                .ports
                .iter()
                .map(|port| parse_port_mapping(port))
                .collect::<anyhow::Result<_>>()?,
            cpus: service.cpus.unwrap_or(2),
            memory_mib: parse_memory_limit(service.mem_limit.as_deref().unwrap_or("512m"))?,
            memory_pool: None,
            shared_kernel_base: None,
            auto_remove: false,
            interactive: service.stdin_open,
            tty: service.tty.then_some([24, 80]),
            network: true,
            network_name: Some(format!("{}_default", self.name)),
            ipv4_address: None,
            peers: None,
        };
        staging::validate_mounts(&mut config)?;
        Ok(config)
    }
}

fn discover_file() -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    for directory in cwd.ancestors() {
        for name in [
            "compose.yaml",
            "compose.yml",
            "docker-compose.yaml",
            "docker-compose.yml",
        ] {
            let file = directory.join(name);
            if file.try_exists()? {
                return Ok(file.canonicalize()?);
            }
        }
    }
    bail!("no Compose file found; use -f FILE")
}

fn project_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"-_".contains(&c))
}

fn dns_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .bytes()
            .last()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
}

fn variable_name(name: &str) -> bool {
    name.bytes()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn expand_value(
    value: &mut serde_yaml::Value,
    variables: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    match value {
        serde_yaml::Value::String(text) => *text = interpolate(text, variables)?,
        serde_yaml::Value::Sequence(values) => {
            for value in values {
                expand_value(value, variables)?;
            }
        }
        serde_yaml::Value::Mapping(map) => {
            map.retain(|key, _| !key.as_str().is_some_and(|key| key.starts_with("x-")));
            for (_, value) in map {
                expand_value(value, variables)?;
            }
        }
        serde_yaml::Value::Tagged(_) => bail!("YAML tags are unsupported"),
        _ => {}
    }
    Ok(())
}

fn escape_config_dollars(value: &mut serde_yaml::Value) {
    match value {
        serde_yaml::Value::String(text) => *text = text.replace('$', "$$"),
        serde_yaml::Value::Sequence(values) => {
            for value in values {
                escape_config_dollars(value);
            }
        }
        serde_yaml::Value::Mapping(map) => {
            // Like interpolation on load, escaping applies to values, not keys.
            for (_, value) in map {
                escape_config_dollars(value);
            }
        }
        _ => {}
    }
}

fn interpolate(text: &str, variables: &BTreeMap<String, String>) -> anyhow::Result<String> {
    let mut output = String::new();
    let mut rest = text;
    while let Some(index) = rest.find('$') {
        output.push_str(&rest[..index]);
        rest = &rest[index + 1..];
        if let Some(after) = rest.strip_prefix('$') {
            output.push('$');
            rest = after;
            continue;
        }
        if !rest.starts_with('{')
            && rest
                .as_bytes()
                .first()
                .is_none_or(|byte| !(byte.is_ascii_alphabetic() || *byte == b'_'))
        {
            // A dollar not beginning a variable (e.g. $5, $!, or a trailing $)
            // is literal Compose text, not an invalid variable expression.
            output.push('$');
            continue;
        }
        let (expression, after) = if let Some(braced) = rest.strip_prefix('{') {
            let end = braced
                .find('}')
                .context("unterminated Compose interpolation")?;
            (&braced[..end], &braced[end + 1..])
        } else {
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            (&rest[..end], &rest[end..])
        };
        let split = expression.find([':', '-', '?']).unwrap_or(expression.len());
        let key = &expression[..split];
        ensure!(variable_name(key), "invalid Compose variable expression");
        let operation = &expression[split..];
        ensure!(
            !operation.contains(['{', '$']),
            "nested Compose interpolation is unsupported"
        );
        let value = variables.get(key);
        match operation {
            "" => output.push_str(value.map(String::as_str).unwrap_or("")),
            op if op.starts_with(":-") => output.push_str(
                value
                    .filter(|value| !value.is_empty())
                    .map(String::as_str)
                    .unwrap_or(&op[2..]),
            ),
            op if op.starts_with('-') => {
                output.push_str(value.map(String::as_str).unwrap_or(&op[1..]))
            }
            op if op.starts_with(":?") || op.starts_with('?') => {
                ensure!(
                    value.is_some_and(|value| !op.starts_with(':') || !value.is_empty()),
                    "required Compose variable {key} is missing"
                );
                output.push_str(value.unwrap());
            }
            _ => bail!("unsupported Compose interpolation operator for {key}"),
        }
        rest = after;
    }
    output.push_str(rest);
    Ok(output)
}

fn read_environment(
    text: &str,
    values: &mut BTreeMap<String, String>,
    overrides: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (key, value) = line
            .split_once('=')
            .context("env file entries must be NAME=VALUE")?;
        let key = key.trim();
        ensure!(variable_name(key), "invalid env file name");
        let value = value.trim();
        let mut variables = values.clone();
        variables.extend(overrides.clone());
        let value = if let Some(single) = value.strip_prefix('\'') {
            single
                .strip_suffix('\'')
                .context("invalid single-quoted env value")?
                .to_owned()
        } else if value.starts_with('"') {
            let decoded: String =
                serde_json::from_str(value).context("invalid double-quoted env value")?;
            interpolate(&decoded, &variables)?
        } else {
            interpolate(
                value
                    .split_once(" #")
                    .map(|(value, _)| value)
                    .unwrap_or(value)
                    .trim_end(),
                &variables,
            )?
        };
        ensure!(!value.contains('\0'), "environment contains NUL");
        values.insert(key.into(), value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn load(directory: &Path, text: &str) -> anyhow::Result<Project> {
        let file = directory.join("compose.yaml");
        fs::write(&file, text)?;
        Project::load(Some(&file), Some("test"))
    }

    #[test]
    fn compose_interpolation_distinguishes_unset_empty_and_escaped_values() {
        let variables =
            BTreeMap::from([("EMPTY".into(), "".into()), ("VALUE".into(), "yes".into())]);
        for (input, expected) in [
            ("${MISSING:-fallback}", "fallback"),
            ("${EMPTY:-fallback}", "fallback"),
            ("${EMPTY-fallback}", ""),
            ("$VALUE/${VALUE?required}/$$VALUE", "yes/yes/$VALUE"),
            ("${EMPTY?required}", ""),
            ("price=$5", "price=$5"),
            ("trailing$", "trailing$"),
            ("$! $- $中文", "$! $- $中文"),
            ("$$5 $VALUE $$$VALUE", "$5 yes $yes"),
        ] {
            assert_eq!(interpolate(input, &variables).unwrap(), expected);
        }
        for input in [
            "${EMPTY:?required}",
            "${MISSING?required}",
            "${VALUE:+alt}",
            "${VALUE",
            "${VALUE:-${OTHER}}",
        ] {
            assert!(interpolate(input, &variables).is_err(), "{input}");
        }
    }

    #[test]
    fn compose_config_roundtrip_preserves_resolved_dollars_in_all_values() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join(".env"), "PVISOR_ROUNDTRIP=first\n").unwrap();
        fs::write(
            directory.path().join("app.env"),
            "FROM_FILE='$PVISOR_ROUNDTRIP'\n",
        )
        .unwrap();
        let project = load(
            directory.path(),
            r#"
services:
  app:
    image: fixture:latest
    build:
      context: ./source-$$PVISOR_ROUNDTRIP
      args: {LITERAL: "$$PVISOR_ROUNDTRIP"}
    command: [sh, -c, "echo $$HOME $${LITERAL} price=$5 $$$$ trailing$"]
    env_file: app.env
    environment:
      LITERAL: "$$PVISOR_ROUNDTRIP"
      RESOLVED: "${PVISOR_ROUNDTRIP}"
      EMPTY: ""
    volumes: ["./data-$$PVISOR_ROUNDTRIP:/data:ro"]
"#,
        )
        .unwrap();
        let hash = project.config_hash("app").unwrap();
        let expected = serde_json::to_value(&project.document).unwrap();
        let output = project.config().unwrap();
        assert_eq!(project.config_hash("app").unwrap(), hash);
        fs::write(directory.path().join(".env"), "PVISOR_ROUNDTRIP=second\n").unwrap();
        fs::remove_file(directory.path().join("app.env")).unwrap();
        let reloaded = load(directory.path(), &output).unwrap();
        assert_eq!(serde_json::to_value(&reloaded.document).unwrap(), expected);
        assert_eq!(reloaded.config_hash("app").unwrap(), hash);
        assert_eq!(reloaded.config().unwrap(), output);
    }

    #[test]
    fn compose_load_resolves_paths_environment_and_dependency_order_without_storage() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join(".env"),
            "PVISOR_COMPOSE_TEST_TAG=3.20\nLITERAL='$NO_EXPANSION'\n",
        )
        .unwrap();
        fs::write(
            directory.path().join("app.env"),
            "KEY=file\nONLY_FILE=kept\n",
        )
        .unwrap();
        let project = load(
            directory.path(),
            r#"
version: "3.8"
x-note: ignored
services:
  app:
    image: alpine:${PVISOR_COMPOSE_TEST_TAG}
    command: echo "two words" '$$LITERAL'
    depends_on: [db]
    env_file: app.env
    environment:
      KEY: explicit
      EMPTY: ""
      PVISOR_COMPOSE_TEST_NOT_SET: null
    volumes: ["./data:/data:ro"]
    ports: ["127.0.0.1:8080:80/tcp"]
  db:
    image: redis:7
"#,
        )
        .unwrap();
        assert_eq!(project.order(&["app".into()], true).unwrap(), ["db", "app"]);
        assert_eq!(project.order(&["app".into()], false).unwrap(), ["app"]);
        let service = &project.document.services["app"];
        assert_eq!(service.image.as_deref(), Some("alpine:3.20"));
        assert_eq!(
            service.command.as_ref().unwrap().words().unwrap(),
            ["echo", "two words", "$LITERAL"]
        );
        assert!(service.volumes[0].starts_with(directory.path().to_str().unwrap()));
        let Environment::Map(environment) = &service.environment else {
            panic!()
        };
        assert_eq!(environment["KEY"].as_deref(), Some("explicit"));
        assert_eq!(environment["ONLY_FILE"].as_deref(), Some("kept"));
        assert_eq!(environment["EMPTY"].as_deref(), Some(""));
        assert_eq!(environment["PVISOR_COMPOSE_TEST_NOT_SET"], None);
        assert!(!project.config().unwrap().contains("env_file"));
        assert!(!directory.path().join("store").exists());
    }

    #[test]
    fn compose_rejects_unknown_configuration_cycles_and_invalid_launch_values() {
        let directory = tempfile::tempdir().unwrap();
        for text in [
            "services: {}",
            "services: {app: {image: alpine, privileged: true}}",
            "services: {app: {build: {context: ., unsupported: true}}}",
            "services: {app: {image: alpine, depends_on: [missing]}}",
            "services: {a: {image: alpine, depends_on: [b]}, b: {image: alpine, depends_on: [a]}}",
            "services: {app: {image: alpine, command: [\"bad\\0arg\"]}}",
            "services: {app: {image: alpine, entrypoint: [\"bad\\0arg\"]}}",
            "services: {app: {image: alpine, ports: ['53:53/udp']}}",
            "services: {app: {image: alpine, volumes: ['named:/data']}}",
            "services: {app: {image: alpine, working_dir: relative}}",
            "services: {app: {image: alpine, cpus: 0}}",
            "services: {app: {image: alpine, mem_limit: 1m}}",
            "services: {app: {image: alpine, environment: {KEY: \"\\0\"}}}",
        ] {
            assert!(load(directory.path(), text).is_err(), "{text}");
        }
    }

    pub(super) fn store(directory: &Path) -> Store {
        let store = Store::open(directory.join("store")).unwrap();
        let context = directory.join("context");
        fs::create_dir(&context).unwrap();
        let file = context.join("Dockerfile");
        fs::write(&file, "FROM scratch\nCMD [\"/bin/true\"]\n").unwrap();
        build::build(
            &store,
            &build::BuildOptions {
                context,
                file,
                tags: vec!["fixture:latest".into()],
                build_args: BTreeMap::new(),
                target: None,
                platform: None,
                pull: false,
                no_cache: false,
                cpus: 1,
                memory_mib: 32,
                network: false,
                timeout: Duration::from_secs(10),
                quiet: true,
                output: None,
            },
        )
        .unwrap();
        store
    }

    fn member(project: &Project, store: &Store) -> Instance {
        store
            .create_project_network(
                NetworkCreate {
                    name: "test_default".into(),
                    subnet: None,
                    gateway: None,
                },
                Some("test"),
            )
            .unwrap();
        store
            .create_member(
                project
                    .create_request("app", store, &store.images().unwrap()[0])
                    .unwrap(),
                &mut |_| {},
                Some(Membership {
                    project: project.name.clone(),
                    service: "app".into(),
                    config_hash: project.config_hash("app").unwrap(),
                    replaces: None,
                    healthcheck: None,
                }),
            )
            .unwrap()
    }

    #[test]
    fn compose_membership_and_service_dns_survive_reopen_and_down_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let project = load(
            directory.path(),
            "services: {app: {image: 'fixture:latest'}}",
        )
        .unwrap();
        let store = store(directory.path());
        let instance = member(&project, &store);
        assert_eq!(
            instance.config.peers.as_ref().unwrap().aliases,
            ["test-app-1", "app"]
        );
        let mut collision = project
            .create_request("app", &store, &instance.image)
            .unwrap();
        collision.name = Some("app".into());
        assert!(
            store
                .create(collision)
                .unwrap_err()
                .to_string()
                .contains("DNS name already allocated")
        );
        fs::write(store.directory(&instance.id).join("stdout.log"), b"ready\n").unwrap();
        let mut output = Vec::new();
        store
            .logs_until(&instance.id, true, &mut output, &mut Vec::new(), &|| true)
            .unwrap();
        assert!(
            output.is_empty(),
            "cancelled readers must not wait for output or owner exit"
        );
        store
            .logs(&instance.id, false, &mut output, &mut Vec::new())
            .unwrap();
        assert_eq!(output, b"ready\n");
        let reopened = Store::open(directory.path().join("store")).unwrap();
        assert_eq!(
            project.instances(&reopened, &[]).unwrap()[0].id,
            instance.id
        );
        // No-recreate keeps the existing container without attempting a VM launch.
        let changed = load(
            directory.path(),
            "services: {app: {image: 'fixture:latest', command: [changed]}}",
        )
        .unwrap();
        let kept = changed
            .reconcile(
                &reopened,
                &[],
                &UpOptions {
                    recreate: RecreatePolicy::Never,
                    ..Default::default()
                },
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(kept[0].id, instance.id);
        assert_eq!(reopened.resolve(&instance.id).unwrap().state, "created");
        project
            .down(&reopened, Duration::ZERO, false, false)
            .unwrap();
        project
            .down(&reopened, Duration::ZERO, false, false)
            .unwrap();
        assert!(reopened.instances(true).unwrap().is_empty());
        assert_eq!(reopened.networks().unwrap().len(), 2);
    }

    #[test]
    fn compose_does_not_adopt_or_remove_same_named_ordinary_containers_or_networks() {
        let directory = tempfile::tempdir().unwrap();
        let project = load(
            directory.path(),
            "services: {app: {image: 'fixture:latest'}}",
        )
        .unwrap();
        let store = store(directory.path());
        store
            .create_network(NetworkCreate {
                name: "test_default".into(),
                subnet: None,
                gateway: None,
            })
            .unwrap();
        let ordinary = store
            .create(
                project
                    .create_request("app", &store, &store.images().unwrap()[0])
                    .unwrap(),
            )
            .unwrap();
        assert!(project.instances(&store, &[]).unwrap().is_empty());
        assert!(
            project
                .up(&store, &[], &UpOptions::default(), &mut |_| {})
                .unwrap_err()
                .to_string()
                .contains("already in use")
        );
        project.down(&store, Duration::ZERO, true, false).unwrap();
        assert_eq!(store.resolve(&ordinary.id).unwrap().state, "created");
        store.remove(&ordinary.id, false).unwrap();
        assert!(
            project
                .up(&store, &[], &UpOptions::default(), &mut |_| {})
                .unwrap_err()
                .to_string()
                .contains("another owner")
        );
        assert_eq!(
            store
                .inspect_network("test_default")
                .unwrap()
                .compose_project,
            None
        );
    }

    #[test]
    fn compose_orphans_require_explicit_removal() {
        let directory = tempfile::tempdir().unwrap();
        let project = load(
            directory.path(),
            "services: {app: {image: 'fixture:latest'}}",
        )
        .unwrap();
        let store = store(directory.path());
        let instance = member(&project, &store);
        let changed = load(
            directory.path(),
            "services: {replacement: {image: 'fixture:latest'}}",
        )
        .unwrap();
        assert!(changed.down(&store, Duration::ZERO, false, false).is_err());
        assert!(store.resolve(&instance.id).is_ok());
        changed.down(&store, Duration::ZERO, true, false).unwrap();
        assert!(store.instances(true).unwrap().is_empty());
    }
}
