//! Restart decisions and exact-generation lifecycle observations.
use super::*;

/// Restart a workload after an observed process exit, while its owner is alive.
/// Admission/transport failures, manual stops and health failures never trigger
/// a retry. There is no boot service: owner/host death requires an explicit start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum RestartPolicy {
    /// Never automatically restart.
    #[default]
    No,
    /// Restart after any workload exit, unless manually stopped.
    Always,
    /// Like Always while the owner lives; manual stop remains stopped.
    UnlessStopped,
    /// Restart nonzero workload exits. Zero retries means unlimited; a workload
    /// running for ten seconds resets the consecutive retry budget and backoff.
    OnFailure {
        /// Maximum consecutive automatic retries, or zero for no limit.
        max_retries: u32,
    },
}

impl std::str::FromStr for RestartPolicy {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> anyhow::Result<Self> {
        Ok(match value {
            "no" => Self::No,
            "always" => Self::Always,
            "unless-stopped" => Self::UnlessStopped,
            "on-failure" => Self::OnFailure { max_retries: 0 },
            value if value.starts_with("on-failure:") => {
                let count = &value["on-failure:".len()..];
                ensure!(
                    !count.is_empty() && count.bytes().all(|c| c.is_ascii_digit()),
                    "invalid restart retry count"
                );
                Self::OnFailure {
                    max_retries: count.parse()?,
                }
            }
            _ => bail!("unsupported restart policy: {value}"),
        })
    }
}

impl TryFrom<String> for RestartPolicy {
    type Error = anyhow::Error;
    fn try_from(value: String) -> anyhow::Result<Self> {
        value.parse()
    }
}
impl From<RestartPolicy> for String {
    fn from(value: RestartPolicy) -> String {
        match value {
            RestartPolicy::No => "no".into(),
            RestartPolicy::Always => "always".into(),
            RestartPolicy::UnlessStopped => "unless-stopped".into(),
            RestartPolicy::OnFailure { max_retries: 0 } => "on-failure".into(),
            RestartPolicy::OnFailure { max_retries } => format!("on-failure:{max_retries}"),
        }
    }
}

pub(super) fn default_stop_timeout() -> u64 {
    10_000
}

pub(super) fn validate(config: &Create) -> anyhow::Result<()> {
    ensure!(
        !config.no_healthcheck || config.healthcheck.is_none(),
        "disabled healthcheck conflicts with an explicit check"
    );
    if let Some(check) = &config.healthcheck {
        check.validate()?;
    }
    ensure!(
        config
            .stop_signal
            .is_none_or(|signal| (1..=64).contains(&signal)),
        "stop signal must be a Linux signal in 1..=64"
    );
    ensure!(
        config.stop_timeout_ms <= 86_400_000,
        "stop grace exceeds one day"
    );
    ensure!(
        config.restart == RestartPolicy::No
            || !config.auto_remove && config.mounts.iter().all(|mount| !mount.staged),
        "restart policies conflict with auto-removal and staged binds"
    );
    ensure!(
        config
            .labels
            .iter()
            .all(|(key, value)| !key.is_empty() && !key.contains('\0') && !value.contains('\0')),
        "labels need a nonempty key and NUL-free key/value"
    );
    Ok(())
}

fn retry(policy: RestartPolicy, code: i32, attempts: u32) -> bool {
    match policy {
        RestartPolicy::No => false,
        RestartPolicy::Always | RestartPolicy::UnlessStopped => true,
        RestartPolicy::OnFailure { max_retries } => {
            code != 0 && (max_retries == 0 || attempts < max_retries)
        }
    }
}

fn stopped(store: &Store, instance: &Instance) -> anyhow::Result<bool> {
    match File::open(store.directory(&instance.id).join("stop-request.json")) {
        Ok(file) => Ok(serde_json::from_reader::<_, String>(file)? == instance.generation),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn supervise(store: &Store, instance: &mut Instance) -> anyhow::Result<()> {
    let mut attempts = 0u32;
    loop {
        let runtime = run_owner_generation(store, instance)?;
        if instance.config.restart == RestartPolicy::No {
            return Ok(());
        }
        let result: Completion = serde_json::from_reader(File::open(
            store
                .root
                .join("results")
                .join(format!("{}.json", instance.id)),
        )?)?;
        if runtime >= Duration::from_secs(10) {
            attempts = 0;
        }
        {
            let _guard = store.lock()?;
            if result.error.is_some()
                || stopped(store, instance)?
                || !retry(instance.config.restart, result.code, attempts)
            {
                return Ok(());
            }
            instance.state = "restarting".into();
            store.save(instance)?;
        }
        // Keep the same owner lease throughout backoff and all successors.
        let delay = Duration::from_millis((100u64 << attempts.min(10)).min(60_000));
        let deadline = Instant::now() + delay;
        while Instant::now() < deadline {
            if stopped(store, instance)? {
                let _guard = store.lock()?;
                instance.state = "exited".into();
                store.save(instance)?;
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _guard = store.lock()?;
        if stopped(store, instance)? {
            instance.state = "exited".into();
            store.save(instance)?;
            return Ok(());
        }
        instance.generation = uuid();
        instance.started_ms = crate::unix_now_ms();
        instance.exit_code = None;
        instance.state = "starting".into();
        instance.restart_count = instance.restart_count.saturating_add(1);
        attempts = attempts.saturating_add(1);
        store.save(instance)?;
    }
}

impl Store {
    pub(super) fn completion(&self, instance: &Instance) -> anyhow::Result<Option<Completion>> {
        let exact = self
            .root
            .join("results")
            .join(format!("{}-{}.json", instance.id, instance.generation));
        let current = self
            .root
            .join("results")
            .join(format!("{}.json", instance.id));
        let file = match File::open(exact).or_else(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                File::open(current)
            } else {
                Err(error)
            }
        }) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let completion: Completion = serde_json::from_reader(file)?;
        Ok((completion.generation == instance.generation).then_some(completion))
    }

    pub(super) fn generation_active(&self, instance: &Instance) -> anyhow::Result<bool> {
        if !self.active(instance)? {
            return Ok(false);
        }
        // Inspect the record directly; resolve/all also reconcile liveness and
        // must not recurse through this generation fence.
        Ok(self
            .read_session::<CachedImage>(&instance.id)?
            .is_some_and(|session| session.active_execution() == instance.generation))
    }

    /// Stop then start this exact container ID, retaining image, writable upper
    /// and volumes. Missing timeout uses its recorded grace. Failed stop never
    /// starts a successor; failure after stop leaves the container stopped.
    pub fn restart(&self, selector: &str, timeout: Option<Duration>) -> anyhow::Result<Instance> {
        let instance = self.resolve(selector)?;
        self.stop(
            &instance.id,
            timeout.unwrap_or(Duration::from_millis(instance.config.stop_timeout_ms)),
            None,
        )?;
        self.start(&instance.id)
    }

    /// Wait for the generation selected at call admission, not a later restart.
    /// Returns the workload exit code as data, including after auto-removal.
    /// A created/unstarted Session is already not running and returns zero,
    /// matching Docker wait; this does not certify any workload execution.
    /// Missing completion after owner death reports an error, never a guessed
    /// success. Receipts remain private in the store until explicitly cleaned.
    pub fn wait(&self, selector: &str) -> anyhow::Result<i32> {
        let instance = self.resolve(selector)?;
        if instance.state == "created" && instance.generation.is_empty() {
            return Ok(0);
        }
        loop {
            let active = self.generation_active(&instance)?;
            if let Some(result) = self.completion(&instance)? {
                return Ok(result.code);
            }
            ensure!(
                active,
                "container owner exited without a completion receipt"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policies_validate_and_bound_retries() {
        for value in [
            "no",
            "always",
            "unless-stopped",
            "on-failure",
            "on-failure:3",
        ] {
            let policy: RestartPolicy = value.parse().unwrap();
            let json = serde_json::to_string(&policy).unwrap();
            assert_eq!(
                serde_json::from_str::<RestartPolicy>(&json).unwrap(),
                policy
            );
        }
        for value in [
            "",
            "yes",
            "always:2",
            "on-failure:-1",
            "on-failure:",
            "on-failure:4294967296",
        ] {
            assert!(value.parse::<RestartPolicy>().is_err(), "{value}");
        }
        let policy = RestartPolicy::OnFailure { max_retries: 2 };
        assert!(!retry(policy, 0, 0));
        assert!(retry(policy, 7, 0));
        assert!(retry(policy, 7, 1));
        assert!(!retry(policy, 7, 2));
    }

    #[test]
    fn exact_receipts_survive_successors_and_old_stop_fences_do_not_target_them() {
        let (_directory, store, _image) = super::super::tests::fixture();
        let mut instance = store.create(super::super::tests::request()).unwrap();
        instance.generation = uuid();
        instance.state = "running".into();
        store.save(&instance).unwrap();
        let first = instance.clone();
        let output = store.output(&instance).unwrap();
        finish_instance(&store, &mut instance, 7, None).unwrap();
        instance.generation = uuid();
        instance.state = "running".into();
        store.save(&instance).unwrap();
        let lease = acquire_lease(&store.directory(&instance.id).join("owner.lock")).unwrap();
        assert!(!store.generation_active(&first).unwrap());
        assert!(store.generation_active(&instance).unwrap());
        assert_eq!(
            output
                .follow(&first.generation, &mut Vec::new(), &mut Vec::new())
                .unwrap(),
            7
        );
        finish_instance(&store, &mut instance, 0, None).unwrap();
        assert_eq!(store.completion(&first).unwrap().unwrap().code, 7);
        drop(lease);
        assert_eq!(store.wait(&instance.id).unwrap(), 0);
        // Reproduce a reader seeing the old running projection after an orderly
        // owner exit: the receipt wins over the historical 137 crash fallback.
        instance.state = "running".into();
        instance.exit_code = None;
        store.save(&instance).unwrap();
        assert_eq!(store.resolve(&instance.id).unwrap().exit_code, Some(0));
    }

    #[test]
    fn lifecycle_defaults_and_restart_conflicts_are_admitted_before_pulling() {
        let (_directory, store, _image) = super::super::tests::fixture();
        let mut request = super::super::tests::request();
        let mut encoded = serde_json::to_value(&request).unwrap();
        for key in ["labels", "restart", "stop_signal", "stop_timeout_ms"] {
            encoded.as_object_mut().unwrap().remove(key);
        }
        let legacy: Create = serde_json::from_value(encoded).unwrap();
        assert_eq!(legacy.stop_timeout_ms, 10_000);
        assert_eq!(legacy.restart, RestartPolicy::No);
        request.restart = RestartPolicy::Always;
        request.auto_remove = true;
        request.image = "unused-invalid-registry-image".into();
        assert!(
            store
                .create(request.clone())
                .unwrap_err()
                .to_string()
                .contains("auto-removal")
        );
        request.auto_remove = false;
        request.stop_signal = Some(65);
        assert!(
            store
                .create(request)
                .unwrap_err()
                .to_string()
                .contains("signal")
        );
        assert!(store.instances(true).unwrap().is_empty());
    }
}
