//! Container compatibility snapshots of the shared durable Session aggregate.
use super::*;
use crate::{Session, SessionKind, SessionState};

/// Admitted persistent container environment and resource references. Identity,
/// selected Execution and lifecycle phase live exclusively in the enclosing
/// Session. This profile cannot create an independent owner or lifecycle record.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerProfile<Backing = CachedImage> {
    name: String,
    image: Image<Backing>,
    config: Create,
    argv: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: PathBuf,
    user: String,
    started_ms: u64,
    exit_code: Option<i32>,
    created_ms: u64,
    compose: Option<compose::Membership>,
    restart_count: u32,
}

impl Store {
    /// Load the actual durable Session behind a container selector. No parallel
    /// container identity/state is persisted. The returned value is a snapshot;
    /// starting, signalling or deleting still requires the runtime's ownership
    /// fences. Task checkpoint/effects APIs do not apply to this profile.
    pub fn session(&self, selector: &str) -> anyhow::Result<Session<ContainerProfile>> {
        let instance = self.resolve(selector)?;
        self.read_session(&instance.id)?
            .context("Session disappeared")
    }

    pub(super) fn read_session<Backing: serde::de::DeserializeOwned>(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<Session<ContainerProfile<Backing>>>> {
        let root = self.directory(id);
        ensure!(
            !root.join("instance.json").try_exists()?,
            "legacy container record requires its owning binary; recreate it as a Session"
        );
        let session = Session::<ContainerProfile<Backing>>::read_profile(&root)?;
        if let Some(session) = &session {
            ensure!(
                session.kind() == SessionKind::Container && session.run_id() == id,
                "foreign container Session binding"
            );
        }
        Ok(session)
    }

    pub(super) fn load_instance<Backing: serde::de::DeserializeOwned>(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<Instance<Backing>>> {
        self.read_session(id)?
            .map(|session| {
                let state = match session.state() {
                    SessionState::Created => "created",
                    SessionState::Starting => "starting",
                    SessionState::Running => "running",
                    SessionState::Restarting => "restarting",
                    SessionState::Terminal => "exited",
                    other => bail!("unsupported container Session phase: {other}"),
                }
                .to_owned();
                let id = session.run_id().to_owned();
                let generation = session.active_execution().to_owned();
                let owner_epoch = session.owner_epoch().to_owned();
                // This is a derived compatibility view, never another persisted state.
                let ContainerProfile {
                    name,
                    image,
                    config,
                    argv,
                    env,
                    cwd,
                    user,
                    started_ms,
                    exit_code,
                    created_ms,
                    compose,
                    restart_count,
                } = session.into_profile();
                Ok(Instance {
                    schema: 1,
                    id,
                    name,
                    image,
                    config,
                    argv,
                    env,
                    cwd,
                    user,
                    state,
                    generation,
                    owner_epoch,
                    started_ms,
                    exit_code,
                    created_ms,
                    compose,
                    restart_count,
                })
            })
            .transpose()
    }

    pub(super) fn save_session<Backing: Serialize + Clone>(
        &self,
        instance: &Instance<Backing>,
    ) -> anyhow::Result<()> {
        let root = self.directory(&instance.id);
        ensure!(
            !root.join("instance.json").try_exists()?,
            "legacy container record is not writable"
        );
        let state = match instance.state.as_str() {
            "created" => SessionState::Created,
            "starting" => SessionState::Starting,
            "running" => SessionState::Running,
            "restarting" => SessionState::Restarting,
            "exited" => SessionState::Terminal,
            _ => bail!("invalid container Session phase"),
        };
        let profile = ContainerProfile {
            name: instance.name.clone(),
            image: instance.image.clone(),
            config: instance.config.clone(),
            argv: instance.argv.clone(),
            env: instance.env.clone(),
            cwd: instance.cwd.clone(),
            user: instance.user.clone(),
            started_ms: instance.started_ms,
            exit_code: instance.exit_code,
            created_ms: instance.created_ms,
            compose: instance.compose.clone(),
            restart_count: instance.restart_count,
        };
        let owner_epoch = if instance.owner_epoch.is_empty() {
            instance.generation.clone()
        } else {
            instance.owner_epoch.clone()
        };
        Session::from_profile(
            instance.id.clone(),
            root,
            instance.generation.clone(),
            state,
            SessionKind::Container,
            owner_epoch,
            profile,
        )?
        .write()
    }
}
