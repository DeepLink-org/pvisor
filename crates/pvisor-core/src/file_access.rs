use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::{
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Identity and approval endpoint bound to one Session view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileAccessContext {
    pub run_id: String,
    pub attempt_id: String,
    pub view: String,
    pub audit_socket: Option<PathBuf>,
}

/// Attempt-local state is not part of the persisted identity or rule value.
#[derive(Debug, Clone, Default, Serialize)]
struct FileAccessBinding {
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<FileAccessContext>,
    // Deserialization always starts armed; persisted input cannot enable bypass.
    #[serde(skip)]
    preparing: Arc<AtomicBool>,
}

/// Validated mount-relative rules and their compiled matchers.
/// Precedence is deny, ask, warn/read, then ordinary access.
/// Rules are immutable so the serialized policy always agrees with authorization.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(try_from = "FileAccessWire")]
pub struct FileAccessPolicy {
    #[serde(flatten)]
    rules: FileAccessRules,
    #[serde(flatten)]
    binding: FileAccessBinding,
    #[serde(skip)]
    deny: GlobSet,
    #[serde(skip)]
    ask: GlobSet,
    #[serde(skip)]
    warn: GlobSet,
    #[serde(skip)]
    allow: GlobSet,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
struct FileAccessWire {
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<FileAccessContext>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    allow: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    layers: Vec<(crate::PolicyScope, FileAccessPolicy)>,
    deny: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ask: Vec<String>,
    warn: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
struct FileAccessRules {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    allow: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    layers: Vec<(crate::PolicyScope, FileAccessPolicy)>,
    deny: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ask: Vec<String>,
    warn: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileAccessDecision {
    Allow,
    Warn,
    Ask,
    Deny,
}

impl PartialEq for FileAccessPolicy {
    fn eq(&self, other: &Self) -> bool {
        self.rules == other.rules && self.binding.context == other.binding.context
    }
}

impl Eq for FileAccessPolicy {}

impl TryFrom<FileAccessWire> for FileAccessPolicy {
    type Error = io::Error;

    fn try_from(rules: FileAccessWire) -> io::Result<Self> {
        let mut policy = Self::new_with_allow(rules.deny, rules.ask, rules.warn, rules.allow)?;
        policy.binding.context = rules.context;
        policy.rules.layers = rules.layers;
        policy
            .rules
            .layers
            .sort_by_key(|(scope, _)| std::cmp::Reverse(*scope));
        Ok(policy)
    }
}

impl FileAccessPolicy {
    /// Compare authorization rules independently of Attempt/audit identities.
    pub fn same_rules(&self, other: &Self) -> bool {
        self.rules.allow == other.rules.allow
            && self.rules.deny == other.rules.deny
            && self.rules.ask == other.rules.ask
            && self.rules.warn == other.rules.warn
            && self.rules.layers.len() == other.rules.layers.len()
            && self.rules.layers.iter().zip(&other.rules.layers).all(
                |((scope, policy), (other_scope, other_policy))| {
                    scope == other_scope && policy.same_rules(other_policy)
                },
            )
    }

    pub fn new(deny: Vec<String>, warn: Vec<String>) -> io::Result<Self> {
        Self::new_with_ask(deny, Vec::new(), warn)
    }

    pub fn new_with_ask(
        deny: Vec<String>,
        ask: Vec<String>,
        warn: Vec<String>,
    ) -> io::Result<Self> {
        Self::new_with_allow(deny, ask, warn, Vec::new())
    }

    pub fn new_with_allow(
        deny: Vec<String>,
        ask: Vec<String>,
        warn: Vec<String>,
        allow: Vec<String>,
    ) -> io::Result<Self> {
        fn compile(patterns: &[String]) -> io::Result<GlobSet> {
            let mut builder = GlobSetBuilder::new();
            for pattern in patterns {
                if pattern.is_empty()
                    || pattern.starts_with('/')
                    || pattern.contains('\0')
                    || pattern
                        .split('/')
                        .any(|part| matches!(part, "" | "." | ".."))
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "file rules must be nonempty mount-relative globs without . or .. components",
                    ));
                }
                builder.add(
                    GlobBuilder::new(pattern)
                        .literal_separator(true)
                        // Conservative on case-insensitive backing filesystems as well.
                        .case_insensitive(true)
                        .backslash_escape(false)
                        .build()
                        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?,
                );
            }
            builder
                .build()
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))
        }
        Ok(Self {
            deny: compile(&deny)?,
            ask: compile(&ask)?,
            warn: compile(&warn)?,
            allow: compile(&allow)?,
            binding: FileAccessBinding::default(),
            rules: FileAccessRules {
                deny,
                ask,
                warn,
                allow,
                layers: Vec::new(),
            },
        })
    }

    pub fn bind_session(&mut self, run_id: &str, attempt_id: &str, view: &str) {
        #[cfg(unix)]
        let audit_socket = crate::audit::socket();
        #[cfg(not(unix))]
        let audit_socket = None;
        self.binding.context = Some(FileAccessContext {
            run_id: run_id.into(),
            attempt_id: attempt_id.into(),
            view: view.into(),
            audit_socket,
        });
        self.binding.preparing = Arc::new(AtomicBool::new(true));
    }
    pub fn for_view(&self, view: &str) -> Self {
        let mut policy = self.clone();
        if let Some(context) = &mut policy.binding.context {
            context.view = view.into();
        }
        policy
    }
    pub fn arm(&self) {
        self.binding.preparing.store(false, Ordering::Release);
    }
    pub fn context(&self) -> Option<&FileAccessContext> {
        self.binding.context.as_ref()
    }

    /// Translate this policy into another view without dropping scope restrictions.
    pub fn prefixed(&self, prefix: &str) -> io::Result<Self> {
        let raw_prefix = prefix;
        let prefix = globset::escape(prefix);
        let translate = |rules: &[String]| {
            rules
                .iter()
                .map(|rule| format!("{prefix}/{rule}"))
                .collect()
        };
        let mut policy = Self::new_with_allow(
            translate(&self.rules.deny),
            translate(&self.rules.ask),
            translate(&self.rules.warn),
            translate(&self.rules.allow),
        )?;
        policy.rules.layers = self
            .rules
            .layers
            .iter()
            .map(|(scope, policy)| Ok((*scope, policy.prefixed(raw_prefix)?)))
            .collect::<io::Result<_>>()?;
        policy.binding = self.binding.clone();
        Ok(policy)
    }
    /// Merge paths projected into the same view, preserving scope identity.
    pub fn extend(&mut self, other: &Self) -> io::Result<()> {
        let mut rules = self.rules.clone();
        rules.deny.extend_from_slice(&other.rules.deny);
        rules.ask.extend_from_slice(&other.rules.ask);
        rules.warn.extend_from_slice(&other.rules.warn);
        rules.allow.extend_from_slice(&other.rules.allow);
        for (scope, policy) in &other.rules.layers {
            if let Some((_, own)) = rules
                .layers
                .iter_mut()
                .find(|(existing, _)| existing == scope)
            {
                own.extend(policy)?;
            } else {
                rules.layers.push((*scope, policy.clone()));
            }
        }
        let mut policy = Self::new_with_allow(rules.deny, rules.ask, rules.warn, rules.allow)?;
        policy.rules.layers = rules.layers;
        policy
            .rules
            .layers
            .sort_by_key(|(scope, _)| std::cmp::Reverse(*scope));
        policy.binding = self.binding.clone();
        *self = policy;
        Ok(())
    }

    pub fn layered<'a>(
        layers: impl IntoIterator<Item = (crate::PolicyScope, &'a Self)>,
        fallback: &Self,
    ) -> Self {
        let mut policy = fallback.clone();
        policy.rules.layers = layers
            .into_iter()
            .map(|(scope, policy)| (scope, policy.clone()))
            .collect();
        policy
            .rules
            .layers
            .sort_by_key(|(scope, _)| std::cmp::Reverse(*scope));
        policy
    }

    /// Stable rule IDs shared by authorization and the admitted plan.
    pub fn rules(&self) -> Vec<(String, String, &'static str)> {
        let mut rules = Vec::new();
        for (scope, policy) in &self.rules.layers {
            rules.extend(policy.rules().into_iter().map(|(id, path, action)| {
                (format!("{scope:?}.{id}").to_lowercase(), path, action)
            }));
        }
        for (action, paths) in [
            ("deny", &self.rules.deny),
            ("ask", &self.rules.ask),
            ("warn", &self.rules.warn),
            ("allow", &self.rules.allow),
        ] {
            rules.extend(
                paths
                    .iter()
                    .enumerate()
                    .map(|(index, path)| (format!("fs.{action}.{index}"), path.clone(), action)),
            );
        }
        rules
    }

    pub fn deny(&self) -> &[String] {
        &self.rules.deny
    }

    pub fn warn(&self) -> &[String] {
        &self.rules.warn
    }

    pub fn ask(&self) -> &[String] {
        &self.rules.ask
    }

    pub fn has_denials(&self) -> bool {
        !self.deny.is_empty()
            || !self.ask.is_empty()
            || self
                .rules
                .layers
                .iter()
                .any(|(_, policy)| policy.has_denials())
    }

    pub fn denied(&self, path: &Path) -> bool {
        self.authorize(path) == FileAccessDecision::Deny
    }

    /// Evaluate a mount-relative path without emitting diagnostics.
    pub fn authorize(&self, path: &Path) -> FileAccessDecision {
        self.rules
            .layers
            .iter()
            .fold(self.local_decision(path), |decision, (_, policy)| {
                decision.max(policy.authorize(path))
            })
    }

    fn local_decision(&self, path: &Path) -> FileAccessDecision {
        if path.ancestors().any(|path| self.deny.is_match(path)) {
            FileAccessDecision::Deny
        } else if path.ancestors().any(|path| self.ask.is_match(path)) {
            FileAccessDecision::Ask
        } else if path.ancestors().any(|path| self.warn.is_match(path)) {
            FileAccessDecision::Warn
        } else {
            FileAccessDecision::Allow
        }
    }

    /// Stable IDs of the rules that actually matched this mount-relative path.
    /// Deny takes precedence over warn, just as it does in `authorize`.
    pub fn matched_rule_ids(&self, path: &Path) -> Vec<String> {
        let decision = self.authorize(path);
        let mut ids = Vec::new();
        for (scope, policy) in &self.rules.layers {
            if policy.authorize(path) == decision {
                ids.extend(
                    policy
                        .matched_rule_ids(path)
                        .into_iter()
                        .map(|id| format!("{scope:?}.{id}").to_lowercase()),
                );
            }
        }
        if self.local_decision(path) == decision {
            ids.extend(self.local_rule_ids(path));
        }
        ids
    }

    fn local_rule_ids(&self, path: &Path) -> Vec<String> {
        fn matches(set: &GlobSet, path: &Path) -> BTreeSet<usize> {
            path.ancestors()
                .flat_map(|part| set.matches(part))
                .collect()
        }
        let deny = matches(&self.deny, path);
        if !deny.is_empty() {
            return deny
                .into_iter()
                .map(|index| format!("fs.deny.{index}"))
                .collect();
        }
        let ask = matches(&self.ask, path);
        if !ask.is_empty() {
            return ask
                .into_iter()
                .map(|index| format!("fs.ask.{index}"))
                .collect();
        }
        let warn = matches(&self.warn, path);
        if warn.is_empty() {
            return matches(&self.allow, path)
                .into_iter()
                .map(|index| format!("fs.allow.{index}"))
                .collect();
        }
        warn.into_iter()
            .map(|index| format!("fs.warn.{index}"))
            .collect()
    }

    /// Enforce the decision and emit path-only diagnostics for warnings/denials.
    pub fn check(&self, path: &Path) -> io::Result<()> {
        match self.authorize(path) {
            FileAccessDecision::Deny => {
                eprintln!("pVisor file access denied: {path:?}");
                Err(io::ErrorKind::PermissionDenied.into())
            }
            FileAccessDecision::Ask => {
                #[cfg(unix)]
                {
                    let prompt = crate::audit::AuditRequest {
                        scope: self.binding.context.as_ref().map(|context| {
                            crate::audit::AuditScope {
                                run_id: context.run_id.clone(),
                                attempt_id: context.attempt_id.clone(),
                                view: context.view.clone(),
                            }
                        }),
                        kind: crate::audit::AuditKind::File,
                        target: path.display().to_string(),
                        reason: format!(
                            "ask file rule: {}",
                            self.matched_rule_ids(path).join(", ")
                        ),
                        host: None,
                        port: None,
                        transport: None,
                    };
                    let decision = if let Some(context) = &self.binding.context {
                        if self.binding.preparing.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        let Some(socket) = &context.audit_socket else {
                            return Err(io::ErrorKind::PermissionDenied.into());
                        };
                        let key =
                            format!("{}:{}:{}", context.run_id, context.attempt_id, context.view);
                        crate::audit::request_at(socket, &key, &prompt)
                    } else {
                        // Standalone adapters retain their process-local approval channel.
                        crate::audit::request(&prompt)
                    };
                    if decision == crate::audit::AuditDecision::Allow {
                        return Ok(());
                    }
                }
                Err(io::ErrorKind::PermissionDenied.into())
            }
            FileAccessDecision::Warn => {
                eprintln!("pVisor sensitive file access warning: {path:?}");
                Ok(())
            }
            FileAccessDecision::Allow => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_rules_ignores_only_audit_bindings_and_checks_scoped_authorization() {
        let mut source = FileAccessPolicy::new(vec!["private/**".into()], vec![]).unwrap();
        source.bind_session("old", "old-attempt", "rootfs");
        let mut restored = source.clone();
        restored.bind_session("new", "new-attempt", "rootfs");
        assert!(source.same_rules(&restored));
        assert_ne!(source, restored);
        let changed = FileAccessPolicy::new(vec!["other/**".into()], vec![]).unwrap();
        assert!(!source.same_rules(&changed));
        let mut policies = crate::SessionPolicies::default();
        policies.user.filesystem = Some(source.clone());
        policies.workspace.filesystem = Some(changed.clone());
        let scoped = policies.filesystem(&FileAccessPolicy::default());
        let mut rebound = scoped.clone();
        rebound.bind_session("new", "new-attempt", "workspace");
        assert!(scoped.same_rules(&rebound));
        assert_ne!(scoped, rebound);
        policies.workspace.filesystem = Some(FileAccessPolicy::default());
        assert!(!scoped.same_rules(&policies.filesystem(&FileAccessPolicy::default())));
        policies.workspace.filesystem = None;
        policies.session.filesystem = Some(changed);
        assert!(!scoped.same_rules(&policies.filesystem(&FileAccessPolicy::default())));
    }

    #[test]
    fn projected_rules_share_only_the_attempt_binding_and_persist_as_values() {
        let mut policy =
            FileAccessPolicy::new_with_ask(vec![], vec!["secret".into()], vec![]).unwrap();
        policy.bind_session("run", "attempt", "workspace");
        let before = serde_json::to_value(&policy).unwrap();
        let mut projected = policy.prefixed("workspace").unwrap().for_view("root");
        projected.extend(&FileAccessPolicy::default()).unwrap();
        assert!(projected.binding.preparing.load(Ordering::Acquire));
        policy.arm();
        assert!(!projected.binding.preparing.load(Ordering::Acquire));
        assert_eq!(serde_json::to_value(&policy).unwrap(), before);
        assert_eq!(policy.context().unwrap().view, "workspace");
        assert_eq!(projected.context().unwrap().view, "root");
        let restored: FileAccessPolicy = serde_json::from_value(before).unwrap();
        assert_eq!(restored, policy);
        assert!(!restored.binding.preparing.load(Ordering::Acquire));
    }

    #[test]
    fn persisted_policy_cannot_restore_preparation_bypass() {
        let mut policy =
            FileAccessPolicy::new_with_ask(vec![], vec!["secret".into()], vec![]).unwrap();
        policy.bind_session("run", "attempt", "workspace");
        assert!(policy.check(Path::new("secret")).is_ok());
        let restored: FileAccessPolicy =
            serde_json::from_value(serde_json::to_value(&policy).unwrap()).unwrap();
        assert_eq!(
            restored.check(Path::new("secret")).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn rules_validate_paths_match_components_and_deny_over_warn() {
        let policy = FileAccessPolicy::new(
            vec!["**/.ssh".into(), "secrets/*.key".into()],
            vec!["**/*.key".into(), "**/.env".into()],
        )
        .unwrap();
        for name in [
            ".ssh/id_rsa",
            "home/me/.ssh/key",
            "HOME/ME/.SSH/key",
            "secrets/a.key",
        ] {
            assert_eq!(policy.authorize(Path::new(name)), FileAccessDecision::Deny);
            assert_eq!(
                policy.check(Path::new(name)).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        for (name, decision) in [
            ("home/me/.ssh-backup", FileAccessDecision::Allow),
            ("secrets/nested/a.key", FileAccessDecision::Warn),
            (".env", FileAccessDecision::Warn),
            ("src/main.rs", FileAccessDecision::Allow),
        ] {
            assert_eq!(policy.authorize(Path::new(name)), decision);
            assert!(policy.check(Path::new(name)).is_ok());
        }
        for glob in [
            "", "/etc/key", "../key", "a/../key", "a//key", "./key", "[", "a\0b",
        ] {
            assert!(
                FileAccessPolicy::new(vec![glob.into()], vec![]).is_err(),
                "{glob:?}"
            );
            assert!(
                FileAccessPolicy::new(vec![], vec![glob.into()]).is_err(),
                "{glob:?}"
            );
        }
    }

    #[test]
    fn serialized_rules_restore_an_executable_policy() {
        let wire = serde_json::json!({"deny": ["**/.ssh"], "warn": ["**/.env"]});
        let policy: FileAccessPolicy = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&policy).unwrap(), wire);
        assert_eq!(policy.clone(), policy);
        assert_eq!(
            policy.authorize(Path::new("home/.ssh/key")),
            FileAccessDecision::Deny
        );
        assert_eq!(
            policy.authorize(Path::new(".env")),
            FileAccessDecision::Warn
        );
        for wire in [serde_json::json!({}), serde_json::json!({"warn": []})] {
            let empty: FileAccessPolicy = serde_json::from_value(wire).unwrap();
            assert_eq!(empty, FileAccessPolicy::default());
            assert!(!empty.has_denials());
            assert_eq!(
                empty.authorize(Path::new(".ssh/key")),
                FileAccessDecision::Allow
            );
        }
        for wire in [
            serde_json::json!({"deny": ["../key"]}),
            serde_json::json!({"warn": ["["]}),
            serde_json::json!({"unknown": []}),
        ] {
            assert!(serde_json::from_value::<FileAccessPolicy>(wire).is_err());
        }
    }

    #[test]
    fn ask_is_a_separate_fail_closed_level_between_deny_and_read() {
        let policy = FileAccessPolicy::new_with_ask(
            vec!["secrets/private.key".into()],
            vec!["secrets/*.key".into()],
            vec!["secrets/**".into()],
        )
        .unwrap();
        assert_eq!(
            policy.authorize(Path::new("secrets/private.key")),
            FileAccessDecision::Deny
        );
        assert_eq!(
            policy.authorize(Path::new("secrets/public.key")),
            FileAccessDecision::Ask
        );
        assert_eq!(
            policy.authorize(Path::new("secrets/note.txt")),
            FileAccessDecision::Warn
        );
        assert_eq!(
            policy.matched_rule_ids(Path::new("secrets/public.key")),
            ["fs.ask.0"]
        );
        assert_eq!(
            policy
                .check(Path::new("secrets/public.key"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        let wire = serde_json::to_value(&policy).unwrap();
        assert_eq!(wire["ask"], serde_json::json!(["secrets/*.key"]));
        assert_eq!(
            serde_json::from_value::<FileAccessPolicy>(wire).unwrap(),
            policy
        );
    }
}
