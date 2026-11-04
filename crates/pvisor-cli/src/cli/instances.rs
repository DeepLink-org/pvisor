//! Shared instance selection for cross-backend lifecycle commands.
use anyhow::{Context, ensure};
use pvisor::{RunRecord, docker};
use std::path::Path;

pub(super) enum Selected {
    Job(Box<RunRecord>),
    Container(Box<docker::Instance>),
}

/// Exact identities/names win over ID prefixes across both backends. Never
/// discard backend errors or choose one of two equally named instances.
pub(super) fn select(store: &docker::Store, selector: &Path) -> anyhow::Result<Selected> {
    if selector.exists() || selector.components().count() > 1 || selector == Path::new("last") {
        return Ok(Selected::Job(Box::new(pvisor::resolve_run(
            Some(selector),
            &pvisor::default_run_home(),
        )?)));
    }
    let selector = selector
        .to_str()
        .context("instance selector must be UTF-8")?;
    let jobs = pvisor::registered_runs()?;
    let containers = store.instances(true)?;
    let exact_job = jobs
        .iter()
        .any(|job| job.run_id == selector || job.agent == selector);
    let exact_container = containers
        .iter()
        .any(|instance| instance.id == selector || instance.name == selector);
    let exact = exact_job || exact_container;
    let mut matches = Vec::new();
    for job in jobs {
        if (exact && (job.run_id == selector || job.agent == selector))
            || (!exact && !selector.is_empty() && job.run_id.starts_with(selector))
        {
            matches.push(Selected::Job(Box::new(job)));
        }
    }
    for instance in containers {
        if (exact && (instance.id == selector || instance.name == selector))
            || (!exact && !selector.is_empty() && instance.id.starts_with(selector))
        {
            matches.push(Selected::Container(Box::new(instance)));
        }
    }
    ensure!(!matches.is_empty(), "No such instance: {selector}");
    ensure!(
        matches.len() == 1,
        "ambiguous instance: {selector}; use its full ID"
    );
    Ok(matches.pop().expect("one match"))
}

pub(super) fn fork(mut args: super::run::ForkArgs) -> anyhow::Result<i32> {
    let source = args.selection().0;
    if source.exists() || source.components().count() > 1 || source == Path::new("last") {
        return super::host_service::call(super::host::JobCommand::Fork(args));
    }
    let store = docker::Store::discover()?;
    match select(&store, args.selection().0)? {
        Selected::Job(record) => {
            args.pin_selection(record.stage_dir());
            super::host_service::call(super::host::JobCommand::Fork(args))
        }
        Selected::Container(instance) => args.fork_container(&store, &instance.id),
    }
}

pub(super) fn remove(
    store: &docker::Store,
    selector: &str,
    force: bool,
    volumes: bool,
) -> anyhow::Result<()> {
    match select(store, Path::new(selector))? {
        Selected::Container(instance) => {
            if force {
                store.stop(&instance.id, std::time::Duration::ZERO, Some("SIGKILL"))?;
            }
            store.remove(&instance.id, volumes)
        }
        Selected::Job(record) => {
            ensure!(!volumes, "--volumes applies only to container instances");
            // Job termination already owns descendant teardown. Never delete a
            // stage merely because a saved PID no longer exists.
            ensure!(
                !force || !pvisor::is_live(&record.stage_dir())?,
                "running Job removal requires pvisor kill ID first"
            );
            record.remove_instance()
        }
    }
}
