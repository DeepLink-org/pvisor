//! Docker-shaped, read-only metadata. Never export private pool/control paths.
use super::*;
use serde_json::{Value, json};

fn timestamp(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms.min(i64::MAX as u64) as i64)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

pub(super) fn deserialize_labels<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Labels {
        Map(BTreeMap<String, Option<String>>),
        List(Vec<String>),
    }
    let labels = match Labels::deserialize(deserializer)? {
        Labels::Map(map) => map
            .into_iter()
            .map(|(k, v)| (k, v.unwrap_or_default()))
            .collect(),
        Labels::List(list) => list
            .into_iter()
            .map(|entry| {
                let (key, value) = entry.split_once('=').unwrap_or((&entry, ""));
                (key.to_owned(), value.to_owned())
            })
            .collect::<BTreeMap<_, _>>(),
    };
    if labels
        .iter()
        .any(|(key, value)| key.is_empty() || key.contains('\0') || value.contains('\0'))
    {
        return Err(serde::de::Error::custom("invalid label key/value"));
    }
    Ok(labels)
}

impl Store {
    /// Resolve a local image selector to its immutable configuration ID. Canonical
    /// repository/tag aliases and unambiguous ID prefixes use the same resolver
    /// as creation. Missing images return None; ambiguity/corruption are errors.
    /// This never pulls and never matches a tag's previous revision.
    pub fn image_id(&self, selector: &str) -> anyhow::Result<Option<String>> {
        Ok(self.local_image(selector)?.map(|image| image.id))
    }

    fn image_configuration(&self, image: &Image) -> anyhow::Result<Value> {
        Ok(serde_json::from_reader(File::open(
            self.image_config_path(image)?,
        )?)?)
    }

    pub(super) fn image_labels(&self, image: &Image) -> anyhow::Result<BTreeMap<String, String>> {
        let config = self.image_configuration(image)?;
        match config.pointer("/config/Labels") {
            None | Some(Value::Null) => Ok(BTreeMap::new()),
            Some(value) => Ok(serde_json::from_value(value.clone())?),
        }
    }

    /// Read Docker-shaped image metadata by local tag, digest or unambiguous ID.
    /// Never pulls. Config contains original OCI defaults (including labels);
    /// private V2 pool paths/handles are not exposed. Output can contain secrets
    /// embedded in image environment or labels.
    pub fn inspect_image(&self, selector: &str) -> anyhow::Result<Value> {
        let image = self.local_image(selector)?.context("No such image")?;
        let original = self.image_configuration(&image)?;
        let tags: Vec<_> = self
            .images()?
            .into_iter()
            .filter(|i| i.id == image.id)
            .map(|i| i.reference)
            .collect();
        let (os, arch) = image
            .platform
            .split_once('/')
            .context("invalid image platform")?;
        let canonical = image_reference_key(&image.reference)?;
        let repository = canonical
            .split_once('@')
            .context("invalid canonical image reference")?
            .0;
        Ok(json!({
            "Id": image.id, "RepoTags": tags, "RepoDigests": [format!("{repository}@{}",image.digest)],
            "Created": original.get("created").cloned().unwrap_or(json!(timestamp(image.created_ms))),
            "Architecture": arch, "Os": os, "Config": original["config"],
            "RootFS": original["rootfs"]
        }))
    }

    /// Read container identity, effective configuration, mounts, ports and current
    /// health in Docker-shaped JSON. This is an observation, not an atomic
    /// snapshot of guest processes. Pid/cgroup quotas are intentionally omitted:
    /// a guest workload is not a host container process. No VM/control/pool
    /// authority is disclosed. Environment and health output can contain secrets.
    pub fn inspect_container(&self, selector: &str) -> anyhow::Result<Value> {
        let instance = self.resolve(selector)?;
        let health = self.health(&instance.id)?;
        let mut state = json!({
            "Status": instance.state, "Running": matches!(instance.state.as_str(),"running"|"restarting"),
            "Restarting": instance.state == "restarting",
            "ExitCode": instance.exit_code.unwrap_or(0),
            "StartedAt": if instance.started_ms == 0 { String::new() } else { timestamp(instance.started_ms) }
        });
        if let Some(health) = health {
            state["Health"] = json!({
                "Status": health.state, "FailingStreak": health.failing_streak,
                "Log": [{"ExitCode": health.exit_code, "Output": health.output}]
            });
        }
        let mounts: Vec<_> = instance.config.mounts.iter().map(|mount| json!({
            "Type": if mount.volume.is_some() { "volume" } else { "bind" },
            "Name": mount.volume.as_deref().unwrap_or("").trim_start_matches("anonymous:"),
            // Do not expose private managed-volume backing to Docker clients.
            "Source": if mount.volume.is_some() { Value::Null } else { json!(mount.source) },
            "Destination": mount.target, "RW": !mount.read_only,
            "Mode": if mount.staged { "stage" } else if mount.read_only { "ro" } else { "rw" }
        })).collect();
        let mut ports = serde_json::Map::new();
        for port in &instance.config.ports {
            ports
                .entry(format!("{}/tcp", port.guest))
                .or_insert(json!([]))
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "HostIp": port.address.to_string(), "HostPort": port.host.to_string()
                }));
        }
        let mut networks = serde_json::Map::new();
        if let Some(name) = &instance.config.network_name {
            networks.insert(name.clone(), json!({
                "IPAddress": instance.config.peers.as_ref().map(|p| p.address.to_string()).unwrap_or_default(),
                "Aliases": instance.config.peers.as_ref().map(|p| p.aliases.clone()).unwrap_or_default()
            }));
        }
        let (policy, retries) = match instance.config.restart {
            RestartPolicy::OnFailure { max_retries } => ("on-failure".to_owned(), max_retries),
            policy => (String::from(policy), 0),
        };
        let entrypoint = match &instance.config.entrypoint {
            None => instance.image.entrypoint.clone(),
            Some(value) if value.is_empty() => vec![],
            Some(value) => vec![value.clone()],
        };
        let command = if !instance.config.command.is_empty() {
            instance.config.command.clone()
        } else if instance.config.entrypoint.is_none() {
            instance.image.cmd.clone()
        } else {
            vec![]
        };
        let health_config = instance.config.healthcheck.as_ref().map(|check| json!({
            "Test":std::iter::once("CMD".to_owned()).chain(check.command.clone()).collect::<Vec<_>>(),
            "Interval":check.interval_ms * 1_000_000,"Timeout":check.timeout_ms * 1_000_000,
            "Retries":check.retries,"StartPeriod":check.start_period_ms * 1_000_000,
            "StartInterval":check.start_interval_ms * 1_000_000,
        }));
        Ok(json!({
            "Id": instance.id, "Name": format!("/{}", instance.name),
            "Created": timestamp(instance.created_ms), "Image": instance.image.id,
            "Path": instance.argv.first(), "Args": instance.argv.get(1..).unwrap_or_default(),
            "State": state, "RestartCount": instance.restart_count,
            "Config": {
                "Image": instance.config.image, "Labels": instance.config.labels,
                "Env": instance.env.iter().map(|(k,v)| format!("{k}={v}")).collect::<Vec<_>>(),
                "WorkingDir": instance.cwd, "User": instance.user,
                "Entrypoint": entrypoint, "Cmd": command,
                "StopSignal": instance.config.stop_signal.unwrap_or(instance.image.stop_signal).to_string(),
                "StopTimeout": instance.config.stop_timeout_ms.div_ceil(1000),
                "Tty": instance.config.tty.is_some(), "OpenStdin": instance.config.interactive
                ,"Healthcheck":health_config
            },
            "HostConfig": {
                "Memory": u64::from(instance.config.memory_mib) * 1024 * 1024,
                "AutoRemove": instance.config.auto_remove,
                "RestartPolicy": {"Name": policy, "MaximumRetryCount": retries}
            },
            "Mounts": mounts, "NetworkSettings": {"Ports": ports, "Networks": networks},
            "Pvisor": {"Kind":"container","SessionId":instance.id,"ExecutionId":instance.generation,
                "OwnerEpoch":instance.owner_epoch,"Generation": instance.generation, "Vcpus": instance.config.cpus}
        }))
    }
}
