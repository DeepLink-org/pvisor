//! Compose local build requests, translated into the existing isolated builder.
use super::*;

/// Controls explicit `compose build`; defaults reuse RUN cache and local bases.
#[derive(Clone, Debug, Default)]
pub struct BuildSettings {
    /// Bypass RUN checkpoint reads; successful steps still refresh the cache.
    pub no_cache: bool,
    /// Refresh registry base images before building.
    pub pull: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum Definition {
    Path(PathBuf),
    Options(Options),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Options {
    #[serde(default = "default_context")]
    context: PathBuf,
    #[serde(default)]
    dockerfile: Option<PathBuf>,
    #[serde(default)]
    args: Environment,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    network: Option<String>,
}

fn default_context() -> PathBuf {
    PathBuf::from(".")
}

impl Definition {
    pub(super) fn resolve(
        &mut self,
        base: &Path,
        variables: &BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        let options = match self {
            Self::Path(path) => Options {
                context: path.clone(),
                dockerfile: None,
                args: Environment::default(),
                target: None,
                network: None,
            },
            Self::Options(options) => options.clone(),
        };
        let context = options.context.to_string_lossy();
        ensure!(
            !context.contains("://") && !context.starts_with("git@"),
            "only local build contexts are supported"
        );
        let context = base.join(options.context);
        let dockerfile = context.join(options.dockerfile.unwrap_or_else(|| "Dockerfile".into()));
        ensure!(
            options
                .network
                .as_deref()
                .is_none_or(|network| ["default", "none"].contains(&network)),
            "unsupported build network"
        );
        let args = match options.args {
            Environment::Map(map) => map,
            Environment::List(list) => list
                .into_iter()
                .map(|entry| match entry.split_once('=') {
                    Some((key, value)) => (key.to_owned(), Some(value.to_owned())),
                    None => (entry, None),
                })
                .collect(),
        };
        let mut resolved = BTreeMap::new();
        for (key, value) in args {
            ensure!(variable_name(&key), "invalid build argument name: {key}");
            if let Some(value) = value.or_else(|| variables.get(&key).cloned()) {
                ensure!(!value.contains('\0'), "build argument contains NUL");
                resolved.insert(key, Some(value));
            }
        }
        *self = Self::Options(Options {
            context,
            dockerfile: Some(dockerfile),
            args: Environment::Map(resolved),
            target: options.target,
            network: options.network,
        });
        Ok(())
    }
}

impl Project {
    pub(super) fn image_name(&self, service: &str) -> String {
        self.document.services[service]
            .image
            .clone()
            .unwrap_or_else(|| format!("{}-{service}:latest", self.name))
    }

    /// Build selected local service images in dependency order, without creating
    /// service containers. Dependencies are only selected when explicitly named.
    /// Image-only services are skipped. Earlier published builds survive failure.
    /// Context paths are local; Dockerfiles are relative to their context.
    /// Build args are not secret storage and can enter the local build cache.
    pub fn build(
        &self,
        store: &Store,
        services: &[String],
        settings: &BuildSettings,
    ) -> anyhow::Result<Vec<Image>> {
        let _lease = self.lease(store)?;
        let mut images = vec![];
        for service in self.order(services, false)? {
            if self.document.services[&service].build.is_some() {
                images.push(self.build_image(store, &service, settings)?);
            }
        }
        Ok(images)
    }

    pub(super) fn build_image(
        &self,
        store: &Store,
        service: &str,
        settings: &BuildSettings,
    ) -> anyhow::Result<Image> {
        let definition = self.document.services[service]
            .build
            .as_ref()
            .context("service has no build definition")?;
        let Definition::Options(options) = definition else {
            unreachable!()
        };
        let Environment::Map(args) = &options.args else {
            unreachable!()
        };
        super::super::build::build(
            store,
            &super::super::build::BuildOptions {
                context: options.context.clone(),
                file: options.dockerfile.clone().unwrap(),
                tags: vec![self.image_name(service)],
                build_args: args
                    .iter()
                    .filter_map(|(key, value)| value.clone().map(|value| (key.clone(), value)))
                    .collect(),
                target: options.target.clone(),
                platform: self.document.services[service].platform.clone(),
                pull: settings.pull,
                no_cache: settings.no_cache,
                cpus: 2,
                memory_mib: 512,
                network: options.network.as_deref() != Some("none"),
                timeout: Duration::from_secs(300),
                quiet: false,
                output: None,
            },
        )
    }

    pub(super) fn prepare_image(
        &self,
        store: &Store,
        service: &str,
        policy: BuildPolicy,
        progress: &mut dyn FnMut(PullProgress),
    ) -> anyhow::Result<Image> {
        let definition = &self.document.services[service];
        let name = self.image_name(service);
        let local = store.local_image(&name)?;
        if definition.build.is_some()
            && (matches!(policy, BuildPolicy::Always)
                || local.is_none() && matches!(policy, BuildPolicy::Missing))
        {
            return self.build_image(store, service, &BuildSettings::default());
        }
        if let Some(image) = local {
            return Ok(image);
        }
        ensure!(
            definition.image.is_some(),
            "service {service} has no local image; run compose build or use up --build"
        );
        // A user-provided image can still be pulled with --no-build.
        match &definition.platform {
            Some(platform) => store.pull_for_platform_with_progress(&name, platform, progress),
            None => store.pull_with_progress(&name, progress),
        }
    }
}
