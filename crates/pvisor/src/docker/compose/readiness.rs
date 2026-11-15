//! Compose health declarations, dependency conditions and bounded readiness.
use super::*;
use health::{Healthcheck, State};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum Condition {
    #[default]
    #[serde(rename = "service_started")]
    Started,
    #[serde(rename = "service_healthy")]
    Healthy,
    #[serde(rename = "service_completed_successfully")]
    CompletedSuccessfully,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Dependency {
    #[serde(default)]
    condition: Condition,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum Dependencies {
    Short(Vec<String>),
    Long(BTreeMap<String, Dependency>),
}
impl Default for Dependencies {
    fn default() -> Self {
        Self::Short(vec![])
    }
}
impl Dependencies {
    pub(super) fn entries(&self) -> Vec<(String, Condition)> {
        match self {
            Self::Short(names) => names
                .iter()
                .map(|name| (name.clone(), Condition::Started))
                .collect(),
            Self::Long(map) => map
                .iter()
                .map(|(name, dependency)| (name.clone(), dependency.condition))
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition {
    #[serde(default)]
    test: Option<Words>,
    #[serde(default)]
    disable: bool,
    #[serde(default)]
    interval: Option<String>,
    #[serde(default)]
    timeout: Option<String>,
    #[serde(default)]
    retries: Option<u32>,
    #[serde(default)]
    start_period: Option<String>,
    #[serde(default)]
    start_interval: Option<String>,
}

impl Definition {
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        if let Some(test) = &self.test {
            test_words(test)?;
        }
        for duration in [&self.interval, &self.timeout, &self.start_interval]
            .into_iter()
            .flatten()
        {
            ensure!(
                milliseconds(duration)? > 0,
                "healthcheck intervals/timeouts must be positive"
            );
        }
        if let Some(duration) = &self.start_period {
            milliseconds(duration)?;
        }
        ensure!(
            self.retries.is_none_or(|retries| retries > 0),
            "healthcheck retries must be positive"
        );
        Ok(())
    }
}

fn test_words(test: &Words) -> anyhow::Result<Vec<String>> {
    let words = match test {
        Words::String(command) => vec!["CMD-SHELL".into(), command.clone()],
        Words::List(words) => words.clone(),
    };
    ensure!(
        matches!(words.first().map(String::as_str), Some("NONE")) && words.len() == 1
            || matches!(words.first().map(String::as_str), Some("CMD"))
                && words.len() > 1
                && !words[1].is_empty()
            || matches!(words.first().map(String::as_str), Some("CMD-SHELL"))
                && words.len() == 2
                && !words[1].is_empty(),
        "healthcheck test must be NONE, CMD with arguments, or CMD-SHELL with a command"
    );
    ensure!(
        words.iter().all(|word| !word.contains('\0')),
        "healthcheck contains NUL"
    );
    Ok(words)
}

pub(in crate::docker) fn milliseconds(text: &str) -> anyhow::Result<u64> {
    ensure!(
        !text.is_empty() && text.len() <= 128,
        "invalid healthcheck duration"
    );
    let mut rest = text;
    let mut nanos = 0.0;
    while !rest.is_empty() {
        let split = rest
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .context("healthcheck duration needs a unit")?;
        let number: f64 = rest[..split]
            .parse()
            .context("invalid healthcheck duration")?;
        rest = &rest[split..];
        let (suffix, scale) = [
            ("ns", 1.0),
            ("us", 1000.0),
            ("ms", 1_000_000.0),
            ("s", 1_000_000_000.0),
            ("m", 60_000_000_000.0),
            ("h", 3_600_000_000_000.0),
        ]
        .into_iter()
        .find(|(suffix, _)| rest.starts_with(suffix))
        .context("unsupported healthcheck duration unit")?;
        nanos += number * scale;
        ensure!(
            nanos.is_finite() && (0.0..=86_400_000_000_000.0).contains(&nanos),
            "healthcheck duration exceeds one day"
        );
        rest = &rest[suffix.len()..];
    }
    Ok((nanos / 1_000_000.0).ceil() as u64)
}

impl Project {
    pub(super) fn resolve_healthcheck(
        &self,
        store: &Store,
        service: &str,
        image: &Image,
    ) -> anyhow::Result<Option<Healthcheck>> {
        let config: serde_json::Value =
            serde_json::from_reader(File::open(store.image_config_path(image)?)?)?;
        effective_healthcheck(
            self.document.services[service].healthcheck.as_ref(),
            &config,
        )
    }

    pub(super) fn validate_health_dependencies(
        &self,
        checks: &BTreeMap<String, bool>,
        dependencies: bool,
    ) -> anyhow::Result<()> {
        if !dependencies {
            return Ok(());
        }
        for service in checks.keys() {
            for (dependency, condition) in self.document.services[service].depends_on.entries() {
                if condition == Condition::Healthy {
                    ensure!(
                        checks.get(&dependency) == Some(&true),
                        "dependency {dependency} requires service_healthy but has no healthcheck"
                    );
                }
            }
        }
        Ok(())
    }

    pub(super) fn is_completion_job(&self, service: &str) -> bool {
        self.document.services.values().any(|definition| {
            definition
                .depends_on
                .entries()
                .iter()
                .any(|(name, condition)| {
                    name == service && *condition == Condition::CompletedSuccessfully
                })
        })
    }

    pub(super) fn wait_dependencies(
        &self,
        store: &Store,
        service: &str,
        deadline: Instant,
    ) -> anyhow::Result<()> {
        for (dependency, condition) in self.document.services[service].depends_on.entries() {
            // Project::up has already started this dependency in topological
            // order. A fast-exiting process need not remain running for the
            // dependent to start. This is distinct from --wait readiness.
            if condition == Condition::Started {
                continue;
            }
            let instances = self.instances(store, std::slice::from_ref(&dependency))?;
            ensure!(
                instances.len() == 1,
                "dependency {dependency} has no unique container"
            );
            self.wait_condition(store, &instances[0], condition, deadline)?;
        }
        Ok(())
    }

    pub(super) fn wait_ready(
        &self,
        store: &Store,
        instance: &Instance,
        deadline: Instant,
    ) -> anyhow::Result<()> {
        let service = &instance.compose.as_ref().unwrap().service;
        let condition = if self.is_completion_job(service) {
            Condition::CompletedSuccessfully
        } else if instance.config.healthcheck.is_some() {
            Condition::Healthy
        } else {
            Condition::Started
        };
        self.wait_condition(store, instance, condition, deadline)
    }

    fn wait_condition(
        &self,
        store: &Store,
        instance: &Instance,
        condition: Condition,
        deadline: Instant,
    ) -> anyhow::Result<()> {
        loop {
            let current = store.resolve(&instance.id)?;
            ensure!(
                current.owner_epoch == instance.owner_epoch
                    && current.compose.as_ref().map(|member| (
                        &member.project,
                        &member.service,
                        &member.config_hash
                    )) == instance.compose.as_ref().map(|member| (
                        &member.project,
                        &member.service,
                        &member.config_hash
                    ))
                    && (current.generation == instance.generation
                        || current.restart_count > instance.restart_count),
                "dependency {} changed Session owner/configuration while waiting",
                current.name
            );
            // Session identity and supervisor ownership remain stable across
            // automatic Execution retries. Wait for that same owner to converge;
            // do not confuse backoff with a terminal dependency failure.
            let retrying = matches!(current.state.as_str(), "starting" | "restarting")
                || current.state == "exited"
                    && current.config.restart != RestartPolicy::No
                    && store.active(&current)?;
            if retrying {
                ensure!(
                    Instant::now() < deadline,
                    "readiness timeout waiting for {}",
                    current.name
                );
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            if condition == Condition::CompletedSuccessfully {
                if current.state == "exited" {
                    ensure!(
                        current.exit_code == Some(0),
                        "dependency {} exited with code {:?}",
                        current.name,
                        current.exit_code
                    );
                    return Ok(());
                }
            } else {
                ensure!(
                    current.state == "running",
                    "dependency {} is {}, exit code {:?}",
                    current.name,
                    current.state,
                    current.exit_code
                );
                if condition == Condition::Started {
                    return Ok(());
                }
                let status = store
                    .health(&instance.id)?
                    .context("service_healthy requires a healthcheck")?;
                if status.generation != current.generation {
                    // Re-observe Session owner/config fences after an automatic
                    // transition between the state and health observations.
                    ensure!(
                        Instant::now() < deadline,
                        "readiness timeout waiting for {}",
                        current.name
                    );
                    continue;
                }
                match status.state {
                    State::Healthy => return Ok(()),
                    State::Unhealthy => bail!(
                        "dependency {} is unhealthy: {}",
                        current.name,
                        status.output
                    ),
                    State::Starting => {}
                }
            }
            ensure!(
                Instant::now() < deadline,
                "readiness timeout waiting for {}",
                current.name
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn effective_healthcheck(
    definition: Option<&Definition>,
    config: &serde_json::Value,
) -> anyhow::Result<Option<Healthcheck>> {
    let parsed = |value: Option<&String>| value.map(|text| milliseconds(text)).transpose();
    let overrides = health::Overrides {
        test: definition
            .and_then(|d| d.test.as_ref())
            .map(test_words)
            .transpose()?,
        disable: definition.is_some_and(|d| d.disable),
        interval: parsed(definition.and_then(|d| d.interval.as_ref()))?,
        timeout: parsed(definition.and_then(|d| d.timeout.as_ref()))?,
        retries: definition.and_then(|d| d.retries),
        start_period: parsed(definition.and_then(|d| d.start_period.as_ref()))?,
        start_interval: parsed(definition.and_then(|d| d.start_interval.as_ref()))?,
    };
    let check = health::resolve(config, overrides)?;
    ensure!(
        check.is_some()
            || definition.is_none()
            || definition.is_some_and(|d| d.disable
                || d.test
                    .as_ref()
                    .is_some_and(|test| test_words(test).is_ok_and(|test| test == ["NONE"]))),
        "healthcheck needs a test or an image default"
    );
    Ok(check)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn health_defaults_override_disable_and_duration_contracts() {
        let config = serde_json::json!({"config":{
            "Shell":["/bin/custom-sh","-c"],
            "Healthcheck":{"Test":["CMD-SHELL","image-check"],"Interval":2000000000,"Retries":0}
        }});
        let inherited = effective_healthcheck(None, &config).unwrap().unwrap();
        assert_eq!(inherited.command, ["/bin/custom-sh", "-c", "image-check"]);
        assert_eq!(inherited.interval_ms, 2000);
        assert_eq!(inherited.retries, 3);
        let definition: Definition =
            serde_yaml::from_str("test: [CMD, check, literal]\ntimeout: 0.25s\ninterval: 1m30s")
                .unwrap();
        definition.validate().unwrap();
        let overridden = effective_healthcheck(Some(&definition), &config)
            .unwrap()
            .unwrap();
        assert_eq!(overridden.command, ["check", "literal"]);
        assert_eq!(overridden.timeout_ms, 250);
        assert_eq!(overridden.interval_ms, 90_000);
        for yaml in ["disable: true", "test: [NONE]"] {
            let disabled: Definition = serde_yaml::from_str(yaml).unwrap();
            assert!(
                effective_healthcheck(Some(&disabled), &config)
                    .unwrap()
                    .is_none()
            );
        }
        for yaml in [
            "test: []",
            "test: [UNKNOWN, cmd]",
            "test: [CMD-SHELL, a, b]",
            "test: [CMD, '']",
            "interval: 0s",
            "timeout: -1s",
            "retries: 0",
        ] {
            let definition: Definition = serde_yaml::from_str(yaml).unwrap();
            assert!(definition.validate().is_err(), "{yaml}");
        }
        assert_eq!(milliseconds("100us").unwrap(), 1);
        assert!(milliseconds("25h").is_err());
    }
}
