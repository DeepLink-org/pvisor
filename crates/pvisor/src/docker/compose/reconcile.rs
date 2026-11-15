//! Admission-first replacement of project services; never infer ownership by name.
use super::*;

struct Plan {
    service: String,
    hash: String,
    previous: Option<Instance>,
    image: Image,
    replace: bool,
    healthcheck: Option<health::Healthcheck>,
}

impl Project {
    pub(super) fn config_hash(&self, service: &str) -> anyhow::Result<String> {
        let mut definition = self.document.services[service].clone();
        // A build definition is an image producer, not a runtime configuration.
        definition.build = None;
        let volumes: BTreeMap<_, _> = definition
            .volumes
            .iter()
            .filter_map(|mount| {
                let key = mount.split(':').next().unwrap();
                (!key.starts_with('/')).then(|| (key, self.volume_name(key)))
            })
            .collect();
        let bytes = if volumes.is_empty() {
            serde_json::to_vec(&definition)?
        } else {
            serde_json::to_vec(&(&definition, volumes))?
        };
        Ok(blake3::hash(&bytes).to_hex().to_string())
    }

    pub(super) fn cleanup_pending(
        &self,
        store: &Store,
        remove_volumes: bool,
    ) -> anyhow::Result<()> {
        for instance in store.instances(true)? {
            if instance
                .compose
                .as_ref()
                .is_some_and(|member| member.project == self.name && member.replaces.is_some())
            {
                ensure!(
                    instance.state == "created" && !store.active(&instance)?,
                    "pending replacement has unexpected lifecycle; preserve it for inspection"
                );
                // Anonymous volumes inherited from the old member remain referenced.
                store.remove(&instance.id, remove_volumes)?;
            }
        }
        Ok(())
    }

    pub(super) fn reconcile(
        &self,
        store: &Store,
        services: &[String],
        options: &UpOptions,
        progress: &mut dyn FnMut(PullProgress),
    ) -> anyhow::Result<Vec<Instance>> {
        // Complete an interrupted promotion if its old record was already
        // removed. Do not discard its links to inherited anonymous volume data.
        let snapshot = store.instances(true)?;
        for candidate in &snapshot {
            if let Some(member) = &candidate.compose
                && member.project == self.name
                && let Some(previous) = &member.replaces
                && !snapshot.iter().any(|old| &old.id == previous)
            {
                ensure!(
                    self.document.services.contains_key(&member.service),
                    "interrupted replacement is now an orphan; inspect it or use compose down"
                );
                self.promote(store, &candidate.id, previous, &member.service)?;
            }
        }
        self.cleanup_pending(store, false)?;
        let order = self.order(services, options.dependencies)?;
        let existing = store.instances(true)?;
        let mut plans = vec![];
        // Reject name/ownership/platform conflicts before builds or volume creation.
        for name in &order {
            let definition = &self.document.services[name];
            if let Some(platform) = &definition.platform {
                require_native_platform(platform)?;
            }
            let members: Vec<_> = existing
                .iter()
                .filter(|instance| {
                    instance.compose.as_ref().is_some_and(|member| {
                        member.project == self.name
                            && member.service == *name
                            && member.replaces.is_none()
                    })
                })
                .collect();
            ensure!(
                members.len() <= 1,
                "multiple containers recorded for service {name}"
            );
            let previous = members.first().map(|instance| (*instance).clone());
            ensure!(
                !existing.iter().any(|instance| Some(&instance.name)
                    == definition.container_name.as_ref()
                    && previous.as_ref().is_none_or(|old| old.id != instance.id)),
                "container name is already in use for service {name}"
            );
            plans.push((name.clone(), previous));
        }
        let mut resolved = vec![];
        for (service, previous) in plans {
            let image = if matches!(options.recreate, RecreatePolicy::Never)
                && !matches!(options.build, BuildPolicy::Always)
                && let Some(previous) = &previous
            {
                previous.image.clone()
            } else {
                self.prepare_image(store, &service, options.build, progress)?
            };
            require_native_platform(&image.platform)?;
            let hash = self.config_hash(&service)?;
            let replace = previous.as_ref().is_some_and(|old| match options.recreate {
                RecreatePolicy::Always => true,
                RecreatePolicy::Never => false,
                RecreatePolicy::Changed => {
                    old.compose.as_ref().unwrap().config_hash != hash
                        || old.image.id != image.id
                        || old.image.platform != image.platform
                }
            });
            let healthcheck = if !replace && let Some(previous) = &previous {
                previous
                    .compose
                    .as_ref()
                    .and_then(|member| member.healthcheck.clone())
            } else {
                self.resolve_healthcheck(store, &service, &image)?
            };
            if previous.is_none() || replace {
                self.document.services[&service].argv(&image)?;
            }
            resolved.push(Plan {
                service,
                previous,
                hash,
                image,
                replace,
                healthcheck,
            });
        }
        self.validate_health_dependencies(
            &resolved
                .iter()
                .map(|plan| (plan.service.clone(), plan.healthcheck.is_some()))
                .collect(),
            options.dependencies,
        )?;
        let network_name = format!("{}_default", self.name);
        if let Some(network) = store
            .networks()?
            .into_iter()
            .find(|network| network.name == network_name)
        {
            ensure!(
                network.compose_project.as_deref() == Some(&self.name),
                "default network belongs to another owner"
            );
        } else {
            store.create_project_network(
                NetworkCreate {
                    name: network_name,
                    subnet: None,
                    gateway: None,
                },
                Some(&self.name),
            )?;
        }
        let creating: Vec<_> = resolved
            .iter()
            .filter(|plan| plan.previous.is_none() || plan.replace)
            .map(|plan| plan.service.clone())
            .collect();
        self.ensure_volumes(store, &creating)?;
        let mut prepared = vec![];
        // Admit every candidate before stopping any previous service. Failure
        // retains pending members for retry/down; no old workload is removed.
        for plan in resolved {
            if !plan.replace
                && let Some(previous) = plan.previous.clone()
            {
                prepared.push((plan, previous));
                continue;
            }
            let mut config = self.create_request(&plan.service, store, &plan.image)?;
            if let Some(previous) = &plan.previous {
                config.name = Some(format!("compose-pending-{}", uuid()));
                for mount in &previous.config.mounts {
                    if mount
                        .volume
                        .as_deref()
                        .is_some_and(|name| name.starts_with("anonymous:"))
                        && !config.mounts.iter().any(|new| new.target == mount.target)
                    {
                        config.mounts.push(mount.clone());
                    }
                }
            }
            let instance = store.create_pinned_member(
                config,
                progress,
                Some(Membership {
                    project: self.name.clone(),
                    service: plan.service.clone(),
                    config_hash: plan.hash.clone(),
                    replaces: plan.previous.as_ref().map(|old| old.id.clone()),
                    healthcheck: plan.healthcheck.clone(),
                }),
                Some(plan.image.clone()),
            )?;
            prepared.push((plan, instance));
        }
        let mut ready = vec![];
        for (plan, instance) in prepared {
            if plan.replace {
                let previous = plan.previous.as_ref().unwrap();
                store.stop(&previous.id, options.timeout, None)?;
                ready.push(self.promote(store, &instance.id, &previous.id, &plan.service)?);
            } else {
                ready.push(instance);
            }
        }
        Ok(ready)
    }

    fn promote(
        &self,
        store: &Store,
        candidate: &str,
        previous_id: &str,
        service: &str,
    ) -> anyhow::Result<Instance> {
        let _guard = store.lock()?;
        let previous = store
            .all()?
            .into_iter()
            .find(|instance| instance.id == previous_id);
        let mut candidate = store.resolve(candidate)?;
        if let Some(previous) = &previous {
            ensure!(
                previous
                    .compose
                    .as_ref()
                    .is_some_and(|member| member.project == self.name
                        && member.service == service
                        && member.replaces.is_none()),
                "replacement source ownership changed"
            );
            ensure!(
                !store.active(previous)? && previous.state != "starting",
                "replacement source lifecycle changed"
            );
        }
        ensure!(
            candidate
                .compose
                .as_ref()
                .is_some_and(|member| member.project == self.name
                    && member.service == service
                    && member.replaces.as_deref() == Some(previous_id)),
            "replacement candidate ownership changed"
        );
        ensure!(
            candidate.state == "created" && !store.active(&candidate)?,
            "replacement lifecycle changed"
        );
        let _stages = previous
            .as_ref()
            .map(|previous| staging::lock_previous(store, previous, true))
            .transpose()?;
        let name = self.document.services[service]
            .container_name
            .as_ref()
            .unwrap();
        ensure!(
            !store
                .all()?
                .iter()
                .any(|instance| instance.id != previous_id
                    && instance.id != candidate.id
                    && instance.name == *name),
            "replacement name is already in use"
        );
        // Keep the admitted candidate's IP while assigning final DNS names.
        let mut config = candidate.config.clone();
        config.ipv4_address = config.peers.as_ref().map(|peer| peer.address);
        config.peers = None;
        config.name = Some(name.clone());
        store.assign_network_excluding(
            &mut config,
            name,
            Some(service),
            &[previous_id, &candidate.id],
        )?;
        candidate.config = config;
        // Persist the validated pending state first. A crash leaves either the
        // old member intact or a disposable unstarted candidate, never an
        // unvalidated workload. Retry/down cleans it without deleting named data.
        store.save(&candidate)?;
        if let Some(previous) = previous {
            store.remove_instance_files(&previous, false)?;
        }
        candidate.name = name.clone();
        candidate.compose.as_mut().unwrap().replaces = None;
        store.save(&candidate)?;
        Ok(candidate)
    }
}
