use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::{io, path::Path};

/// Validated mount-relative rules and their compiled matchers.
/// Precedence is deny, ask, warn/read, then ordinary access.
/// Rules are immutable so the serialized policy always agrees with authorization.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(try_from = "FileAccessRules")]
pub struct FileAccessPolicy {
    #[serde(flatten)]
    rules: FileAccessRules,
    #[serde(skip)]
    deny: GlobSet,
    #[serde(skip)]
    ask: GlobSet,
    #[serde(skip)]
    warn: GlobSet,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
struct FileAccessRules {
    deny: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ask: Vec<String>,
    warn: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAccessDecision {
    Allow,
    Warn,
    Ask,
    Deny,
}

impl PartialEq for FileAccessPolicy {
    fn eq(&self, other: &Self) -> bool {
        self.rules == other.rules
    }
}

impl Eq for FileAccessPolicy {}

impl TryFrom<FileAccessRules> for FileAccessPolicy {
    type Error = io::Error;

    fn try_from(rules: FileAccessRules) -> io::Result<Self> {
        Self::new_with_ask(rules.deny, rules.ask, rules.warn)
    }
}

impl FileAccessPolicy {
    pub fn new(deny: Vec<String>, warn: Vec<String>) -> io::Result<Self> {
        Self::new_with_ask(deny, Vec::new(), warn)
    }

    pub fn new_with_ask(
        deny: Vec<String>,
        ask: Vec<String>,
        warn: Vec<String>,
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
            rules: FileAccessRules { deny, ask, warn },
        })
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
        !self.deny.is_empty() || !self.ask.is_empty()
    }

    pub fn denied(&self, path: &Path) -> bool {
        path.ancestors().any(|path| self.deny.is_match(path))
    }

    /// Evaluate a mount-relative path without emitting diagnostics.
    pub fn authorize(&self, path: &Path) -> FileAccessDecision {
        if self.denied(path) {
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
        matches(&self.warn, path)
            .into_iter()
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
                if crate::audit::configured() && !crate::audit::enabled() {
                    // The supervisor may inspect the overlay while preparing
                    // the Job, before the Agent exists to authorize.
                    return Ok(());
                }
                #[cfg(unix)]
                if crate::audit::enabled() {
                    if crate::audit::request(&crate::audit::AuditRequest {
                        kind: crate::audit::AuditKind::File,
                        target: path.display().to_string(),
                        reason: format!(
                            "ask file rule: {}",
                            self.matched_rule_ids(path).join(", ")
                        ),
                        host: None,
                        port: None,
                        transport: None,
                    }) != crate::audit::AuditDecision::Allow
                    {
                        return Err(io::ErrorKind::PermissionDenied.into());
                    }
                    return Ok(());
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
