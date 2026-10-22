//! Shim-level payload injection: generic primitives, data-driven payloads.
//!
//! The shim injects nothing by itself. A node-side catalogue
//! (`/etc/pvisor-shim/injection.json`, override with
//! `PVISOR_SHIM_INJECTION_CONFIG`) names payloads — a host binary, the
//! absolute path it occupies inside the container, optional argv and
//! environment — and the `io.pvisor.inject` bundle annotation selects
//! payloads by name (comma separated). With no annotation no injection
//! happens and no code path differs from an unmodified task.
//!
//! Admission is fail-closed, like the executor tier vocabulary: an unknown
//! payload name, a duplicate name or target, a target already occupied in
//! the image (any file, directory or symlink), a missing or non-regular
//! source, or a target claimed by a spec/request mount rejects the task
//! instead of silently degrading it.
//!
//! Injected processes are advisory siblings, never the workload's parent:
//! the user command stays PID 1 with native signal and exit semantics, and
//! a sibling's death only reaches its log. The container tier bind-mounts
//! each payload read-only into the rootfs and forks one sibling per payload
//! through the same namespace/pivot dance as the init child (see
//! [`crate::child`]); the VM tier copies the binary into the rootfs and the
//! guest supervisor starts it from the launch config (see [`crate::vm`]).
//! The shim knows nothing about payload internals — no ports, protocols or
//! versions; "which binary, where, with what argv/env" is entirely
//! catalogue data, so removing a payload is a configuration change.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::plan::{MountPlan, normalize_absolute};
use crate::spec::ANNOTATION_PREFIX;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Annotation selecting payload names (comma separated), given without the
/// `io.pvisor.` prefix.
pub const INJECT_ANNOTATION: &str = "inject";

/// Default catalogue location; `PVISOR_SHIM_INJECTION_CONFIG` overrides it.
pub const DEFAULT_CATALOGUE_PATH: &str = "/etc/pvisor-shim/injection.json";

/// Root for per-task injection logs on the host;
/// `PVISOR_SHIM_INJECTION_LOG_DIR` overrides it.
pub const DEFAULT_LOG_ROOT: &str = "/var/log/pvisor-shim";

#[derive(Debug, Error)]
pub enum InjectionError {
    #[error("io.pvisor.inject is invalid: {0}")]
    Annotation(String),
    #[error("injection catalogue {path} cannot be read: {reason}")]
    Catalogue { path: PathBuf, reason: String },
    #[error("injection payload {name:?} is invalid: {reason}")]
    Payload { name: String, reason: String },
    #[error(
        "unknown injection payload {name:?}; configured payloads: {}",
        known.join(", ")
    )]
    UnknownPayload { name: String, known: Vec<String> },
    #[error("injection payload {name:?} repeats target {target} of payload {other:?}")]
    DuplicateTarget {
        name: String,
        other: String,
        target: PathBuf,
    },
    #[error("injection payload {name:?} source path {path} is missing or not a regular file")]
    Source { name: String, path: PathBuf },
    #[error(
        "injection payload {name:?} target {target} already exists in the image; \
         refusing to overwrite image content"
    )]
    TargetOccupied { name: String, target: PathBuf },
    #[error("injection payload {name:?} target {target} is claimed by a container mount")]
    TargetMounted { name: String, target: PathBuf },
}

/// One catalogue entry: what to inject and how to run it. The catalogue is
/// node data (distributed and versioned by the operator, e.g. a DaemonSet);
/// the shim only reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadSpec {
    /// Absolute host path of the payload binary.
    pub source: PathBuf,
    /// Absolute path the binary occupies inside the container. Must be free
    /// in the image; admission rejects occupied targets.
    pub target: PathBuf,
    /// Payload argv; defaults to `[target]`. argv[0] need not equal the
    /// target (display-name conventions are the payload's business).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    /// Environment overlaid on the workload environment.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Stage the file only, without spawning a sibling (default `true`
    /// runs it). Entry-point wrappers and files the workload itself
    /// starts use `false`.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub run: bool,
    /// VM tier only: bridge this guest TCP port to the pod network —
    /// the guest supervisor relays AF_VSOCK (same port number) to guest
    /// `127.0.0.1:port`, and the shim exposes a listener inside the pod
    /// network namespace. The container tier ignores it (the sibling
    /// already shares the pod network).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge: Option<u16>,
    /// VM tier only: stage through the catalogue's shared read-only
    /// directory instead of copying into the container's writable layer —
    /// the guest sees one host-wide copy through a read-only virtio-fs
    /// share and the target becomes a symlink. Requires `share_dir` and a
    /// source directly inside it. The container tier ignores the flag
    /// (bind mounts already share the host page cache).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub share: bool,
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

/// The catalogue file: `{"share_dir": ..., "payloads": {name: PayloadSpec}}`.
/// `share_dir` (optional) is the host directory shared read-only into VM
/// guests for `share` payloads; one host copy serves every sandbox.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InjectionCatalogue {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_dir: Option<PathBuf>,
    pub payloads: BTreeMap<String, PayloadSpec>,
}

/// A resolved payload as carried in [`crate::plan::ContainerPlan`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InjectionPlan {
    pub name: String,
    pub source: PathBuf,
    pub target: PathBuf,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// False stages the file without spawning a sibling.
    pub run: bool,
    /// VM-tier port bridge into the pod network (see [`PayloadSpec`]).
    pub bridge: Option<u16>,
    /// The catalogue's shared directory when this payload stages through
    /// the read-only virtio-fs share (see [`PayloadSpec::share`]).
    pub share: Option<PathBuf>,
    /// Set when the payload was already delivered through a pod volume's
    /// host source (upstream's emptyDir mounts shadow rootfs staging in
    /// VM guests); the VM runner then skips staging it again.
    #[serde(default)]
    pub staged: bool,
}

/// Where the catalogue is read from on this node.
pub fn catalogue_path() -> PathBuf {
    std::env::var("PVISOR_SHIM_INJECTION_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_CATALOGUE_PATH))
}

/// Where per-task injection logs are written on this node.
pub fn log_root() -> PathBuf {
    std::env::var("PVISOR_SHIM_INJECTION_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_LOG_ROOT))
}

fn invalid_payload(name: &str, reason: impl Into<String>) -> InjectionError {
    InjectionError::Payload {
        name: name.to_string(),
        reason: reason.into(),
    }
}

/// Payload names become log file names, so they are restricted to a simple
/// charset (alphanumeric start, then alphanumerics, `.`, `_`, `-`) and a
/// bounded length. Everything stricter — or looser — is data, not code.
fn valid_payload_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn validate_payload(
    name: &str,
    spec: &PayloadSpec,
    share_dir: Option<&Path>,
) -> Result<(), InjectionError> {
    if !valid_payload_name(name) {
        return Err(invalid_payload(
            name,
            "payload names start with an alphanumeric and use only \
             alphanumerics, '.', '_' and '-' (max 64 bytes)",
        ));
    }
    if !spec.source.is_absolute() {
        return Err(invalid_payload(
            name,
            "source must be an absolute host path",
        ));
    }
    if !spec.target.is_absolute() || spec.target == Path::new("/") {
        return Err(invalid_payload(
            name,
            "target must be an absolute container path other than /",
        ));
    }
    // Lexical normalization rejects `..` traversal and `.` segments; the
    // normalized form is what later stages stage and mount.
    let normalized = normalize_absolute(&spec.target);
    if normalized != spec.target {
        return Err(invalid_payload(
            name,
            format!(
                "target must already be normalized (got {})",
                spec.target.display()
            ),
        ));
    }
    if !spec.argv.is_empty()
        && spec
            .argv
            .iter()
            .any(|word| word.is_empty() || word.contains('\0'))
    {
        return Err(invalid_payload(
            name,
            "argv words must be non-empty and NUL-free",
        ));
    }
    if spec
        .env
        .keys()
        .any(|key| key.is_empty() || key.contains('=') || key.contains('\0'))
        || spec.env.values().any(|value| value.contains('\0'))
    {
        return Err(invalid_payload(
            name,
            "environment names must be non-empty and free of '=' and NUL; \
             values must be NUL-free",
        ));
    }
    if let Some(port) = spec.bridge
        && port == 0
    {
        return Err(invalid_payload(name, "bridge port must be 1-65535"));
    }
    if spec.share {
        let Some(share_dir) = share_dir else {
            return Err(invalid_payload(
                name,
                "share requires the catalogue's share_dir",
            ));
        };
        if spec.source.parent() != Some(share_dir) {
            return Err(invalid_payload(
                name,
                "shared sources must sit directly inside share_dir",
            ));
        }
    }
    Ok(())
}

/// Parse the catalogue from bytes (tests feed fragments directly). Only
/// the reason is reported; the caller attaches the file path context.
pub fn parse_catalogue(bytes: &[u8]) -> Result<InjectionCatalogue, String> {
    let catalogue: InjectionCatalogue =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if let Some(share_dir) = &catalogue.share_dir
        && !share_dir.is_absolute()
    {
        return Err(format!(
            "share_dir must be absolute, got {}",
            share_dir.display()
        ));
    }
    for (name, spec) in &catalogue.payloads {
        validate_payload(name, spec, catalogue.share_dir.as_deref())
            .map_err(|error| error.to_string())
            .map_err(|reason| format!("payload {name:?}: {reason}"))?;
    }
    Ok(catalogue)
}

/// Split the annotation value into payload names. Strict: empty segments
/// (dangling commas, whitespace-only entries) are typos and reject.
fn parse_annotation(value: &str) -> Result<Vec<String>, InjectionError> {
    let mut names = Vec::new();
    for entry in value.split(',') {
        let name = entry.trim();
        if name.is_empty() {
            return Err(InjectionError::Annotation(format!(
                "empty payload name in {value:?} (comma-separated names expected)"
            )));
        }
        names.push(name.to_string());
    }
    Ok(names)
}

/// Resolve the task's injections. Pure catalog logic plus host admission
/// checks (source regular file, target free in the rootfs, target unclaimed
/// by mounts); kept separate from [`Self::resolve`] so tests can supply the
/// catalogue inline.
pub fn resolve_against(
    annotation: Option<&str>,
    catalogue: &InjectionCatalogue,
    rootfs: &Path,
    mounts: &[MountPlan],
) -> Result<Vec<InjectionPlan>, InjectionError> {
    let Some(raw) = annotation else {
        return Ok(Vec::new());
    };
    let names = parse_annotation(raw)?;
    let mounted: HashSet<&Path> = mounts.iter().map(|m| m.destination.as_path()).collect();
    let mut plans = Vec::with_capacity(names.len());
    let mut seen: HashMap<&str, &Path> = HashMap::new();
    for name in &names {
        let spec = catalogue.payloads.get(name).ok_or_else(|| {
            let mut known: Vec<String> = catalogue.payloads.keys().cloned().collect();
            known.sort();
            InjectionError::UnknownPayload {
                name: name.clone(),
                known,
            }
        })?;
        if let Some((other, target)) = seen
            .iter()
            .find_map(|(other, target)| (target == &spec.target).then_some((*other, *target)))
        {
            return Err(InjectionError::DuplicateTarget {
                name: name.clone(),
                other: other.to_string(),
                target: target.to_path_buf(),
            });
        }
        if let Some(existing) = mounted.get(spec.target.as_path()) {
            return Err(InjectionError::TargetMounted {
                name: name.clone(),
                target: existing.to_path_buf(),
            });
        }
        if !std::fs::metadata(&spec.source)
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            return Err(InjectionError::Source {
                name: name.clone(),
                path: spec.source.clone(),
            });
        }
        let relative = spec.target.strip_prefix("/").expect("validated absolute");
        if std::fs::symlink_metadata(rootfs.join(relative)).is_ok() {
            return Err(InjectionError::TargetOccupied {
                name: name.clone(),
                target: spec.target.clone(),
            });
        }
        seen.insert(name.as_str(), &spec.target);
        plans.push(InjectionPlan {
            name: name.clone(),
            source: spec.source.clone(),
            target: spec.target.clone(),
            argv: if spec.argv.is_empty() {
                vec![spec.target.to_string_lossy().to_string()]
            } else {
                spec.argv.clone()
            },
            env: spec.env.clone(),
            run: spec.run,
            bridge: spec.bridge,
            share: spec
                .share
                .then(|| catalogue.share_dir.clone().expect("validated")),
            staged: false,
        });
    }
    Ok(plans)
}

/// Resolve injections for a task from its annotations, the node catalogue
/// and its rootfs. An absent annotation resolves to no injections; anything
/// requested but not satisfiable rejects the task.
pub fn resolve(
    annotations: &HashMap<String, String>,
    rootfs: &Path,
    mounts: &[MountPlan],
) -> Result<Vec<InjectionPlan>, InjectionError> {
    let annotation = annotations.get(&format!("{ANNOTATION_PREFIX}{INJECT_ANNOTATION}"));
    let Some(raw) = annotation else {
        return Ok(Vec::new());
    };
    if raw.trim().is_empty() {
        return Err(InjectionError::Annotation(
            "io.pvisor.inject is present but empty; remove it or list payload names".into(),
        ));
    }
    let path = catalogue_path();
    let bytes = std::fs::read(&path).map_err(|error| InjectionError::Catalogue {
        path: path.clone(),
        reason: error.to_string(),
    })?;
    let catalogue = parse_catalogue(&bytes).map_err(|reason| InjectionError::Catalogue {
        path: path.clone(),
        reason,
    })?;
    resolve_against(Some(raw), &catalogue, rootfs, mounts)
}

/// Bind-mount plans for the container tier: one read-only, non-suid,
/// non-dev bind per payload, appended to the spec mounts so the init parent
/// materializes them before the children fork. `noexec` is deliberately
/// absent — the payload must be executable.
pub fn bind_mount_plans(injections: &[InjectionPlan]) -> Vec<MountPlan> {
    injections
        .iter()
        .map(|injection| MountPlan {
            destination: injection.target.clone(),
            fs_type: "bind".to_string(),
            source: Some(injection.source.to_string_lossy().to_string()),
            options: vec!["ro".to_string(), "nosuid".to_string(), "nodev".to_string()],
            from_request: false,
        })
        .collect()
}

/// Environment for an injected sibling: the workload environment with the
/// payload environment overlaid (payload wins on conflicts). Workload
/// entries without `=` cannot be keyed and are dropped from the sibling
/// environment; the workload's own execve still receives them verbatim.
pub fn merged_env(base: &[String], overlay: &BTreeMap<String, String>) -> Vec<String> {
    let mut merged: BTreeMap<String, String> = base
        .iter()
        .filter_map(|entry| {
            entry
                .split_once('=')
                .map(|(key, value)| (key.to_string(), value.to_string()))
        })
        .collect();
    for (key, value) in overlay {
        merged.insert(key.clone(), value.clone());
    }
    merged
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect()
}

/// Open the host-side descriptors handed to the injected siblings:
/// `[null, log_0, log_1, ...]` — one null descriptor plus one log file per
/// payload under `<log root>/<task id>/inject-<name>.log`. The list length
/// doubles as the number of start-gate waiters (init child plus siblings).
///
/// All descriptors are cleared of `FD_CLOEXEC` so they survive the
/// self-exec into the internal parent (same handoff as the workload
/// stdio). A log file that cannot be opened degrades to the null
/// descriptor with a warning: logging is best-effort diagnostics and must
/// not gate task creation.
#[cfg(target_os = "linux")]
pub fn open_task_fds(task_id: &str, injections: &[InjectionPlan]) -> anyhow::Result<Vec<i32>> {
    use std::os::fd::AsRawFd;

    let running = injections.iter().filter(|injection| injection.run).count();
    let mut fds = Vec::with_capacity(running + 1);
    let null = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .map_err(|error| anyhow::anyhow!("open /dev/null: {error}"))?;
    // std opens with O_CLOEXEC; the internal parent must inherit.
    unsafe { libc::fcntl(null.as_raw_fd(), libc::F_SETFD, 0) };
    let null_fd = null.as_raw_fd();
    std::mem::forget(null);
    fds.push(null_fd);

    let dir = log_root().join(task_id);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        log::warn!(
            "cannot create injection log directory {}: {error}; \
             injected payloads will log to /dev/null",
            dir.display()
        );
    }
    // One log descriptor per RUNNING sibling: stage-only payloads never
    // spawn, so they neither wait on the start pipe nor need a log.
    for injection in injections.iter().filter(|injection| injection.run) {
        let path = dir.join(format!("inject-{}.log", injection.name));
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path);
        match log {
            Ok(file) => {
                unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) };
                fds.push(file.as_raw_fd());
                std::mem::forget(file);
            }
            Err(error) => {
                log::warn!(
                    "cannot open injection log {}: {error}; payload {} logs to /dev/null",
                    path.display(),
                    injection.name
                );
                fds.push(null_fd);
            }
        }
    }
    Ok(fds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalogue(json: &str) -> InjectionCatalogue {
        parse_catalogue(json.as_bytes()).unwrap()
    }

    fn catalogue_error(json: &str) -> String {
        parse_catalogue(json.as_bytes()).unwrap_err()
    }

    fn spec_json() -> String {
        r#"{"payloads":{
            "execd":{"source":"/opt/pvisor/execd","target":"/opt/opensandbox/execd",
                     "env":{"EXECD_PORT":"44772"}},
            "tool":{"source":"/opt/pvisor/tool","target":"/usr/local/bin/tool",
                    "argv":["/usr/local/bin/tool","--serve"]}
        }}"#
        .to_string()
    }

    #[test]
    fn catalogue_parses_and_rejects_unknown_fields() {
        let parsed = catalogue(&spec_json());
        assert_eq!(parsed.payloads.len(), 2);
        assert!(parse_catalogue(br#"{"payloads":{}}"#).is_ok());
        assert!(parse_catalogue(br#"{"unknown":1}"#).is_err());
        assert!(
            parse_catalogue(br#"{"payloads":{"a":{"source":"/s","target":"/t","bogus":1}}}"#)
                .is_err()
        );
    }

    #[test]
    fn catalogue_rejects_invalid_payload_definitions() {
        for (name, json) in [
            (
                "relative source",
                r#"{"payloads":{"a":{"source":"s","target":"/t"}}}"#,
            ),
            (
                "relative target",
                r#"{"payloads":{"a":{"source":"/s","target":"t"}}}"#,
            ),
            (
                "root target",
                r#"{"payloads":{"a":{"source":"/s","target":"/"}}}"#,
            ),
            (
                "unnormalized target",
                r#"{"payloads":{"a":{"source":"/s","target":"/opt/../t"}}}"#,
            ),
            (
                "bad name",
                r#"{"payloads":{"../x":{"source":"/s","target":"/t"}}}"#,
            ),
            (
                "empty argv word",
                r#"{"payloads":{"a":{"source":"/s","target":"/t","argv":[""]}}}"#,
            ),
            (
                "env name with =",
                r#"{"payloads":{"a":{"source":"/s","target":"/t","env":{"A=B":"1"}}}}"#,
            ),
        ] {
            let error = parse_catalogue(json.as_bytes()).unwrap_err();
            assert!(error.contains("invalid"), "{name}: {error}");
        }
    }

    #[test]
    fn absent_annotation_injects_nothing() {
        let rootfs = tempfile::tempdir().unwrap();
        let empty = resolve_against(None, &catalogue(&spec_json()), rootfs.path(), &[]).unwrap();
        assert!(empty.is_empty());
    }

    fn staged_rootfs() -> (tempfile::TempDir, std::path::PathBuf) {
        let rootfs = tempfile::tempdir().unwrap();
        let source = rootfs.path().join("payload");
        std::fs::write(&source, b"#!/bin/true\n").unwrap();
        // The source lives outside the rootfs; point the catalogue at it.
        let source = source.canonicalize().unwrap();
        (rootfs, source)
    }

    fn catalogue_with_source(source: &Path) -> InjectionCatalogue {
        let json = format!(
            r#"{{"payloads":{{"execd":{{"source":{},"target":"/opt/opensandbox/execd"}}}}}}"#,
            serde_json::to_string(&source).unwrap()
        );
        catalogue(&json)
    }

    #[test]
    fn unknown_or_malformed_annotations_fail_closed() {
        let rootfs = tempfile::tempdir().unwrap();
        let parsed = catalogue(&spec_json());
        for raw in [
            "missing-payload",
            "execd,",
            ",execd",
            " execd , ",
            "execd,execd-duplicate-typo",
        ] {
            let error = resolve_against(Some(raw), &parsed, rootfs.path(), &[]).unwrap_err();
            assert!(!error.to_string().is_empty(), "{raw}: {error}");
        }
        // Whitespace around a real name is trimmed; the resolution then
        // proceeds to the host checks (this catalogue's fake sources fail
        // there, which proves the annotation itself was accepted).
        let error = resolve_against(Some(" execd "), &parsed, rootfs.path(), &[]).unwrap_err();
        assert!(matches!(error, InjectionError::Source { .. }));
    }

    #[test]
    fn unknown_payload_lists_known_names() {
        let rootfs = tempfile::tempdir().unwrap();
        let error = resolve_against(Some("nope"), &catalogue(&spec_json()), rootfs.path(), &[])
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("nope"), "{message}");
        assert!(message.contains("execd"), "{message}");
    }

    #[test]
    fn duplicate_targets_are_rejected_once_named_twice_or_across_payloads() {
        let rootfs = tempfile::tempdir().unwrap();
        let (staging, source) = staged_rootfs();
        let json = format!(
            r#"{{"payloads":{{
                "a":{{"source":{s},"target":"/opt/x"}},
                "b":{{"source":{s},"target":"/opt/x"}}
            }}}}"#,
            s = serde_json::to_string(&source).unwrap()
        );
        let parsed = catalogue(&json);
        let error = resolve_against(Some("a,b"), &parsed, rootfs.path(), &[]).unwrap_err();
        assert!(error.to_string().contains("/opt/x"), "{error}");
        // The same payload twice also collides with itself.
        let error = resolve_against(Some("a,a"), &parsed, rootfs.path(), &[]).unwrap_err();
        assert!(matches!(error, InjectionError::DuplicateTarget { .. }));
        drop(staging);
    }

    #[test]
    fn occupied_targets_and_claimed_mounts_are_rejected() {
        let (rootfs, source) = staged_rootfs();
        let parsed = &catalogue_with_source(&source);
        std::fs::create_dir_all(rootfs.path().join("opt/opensandbox")).unwrap();
        std::fs::write(rootfs.path().join("opt/opensandbox/execd"), b"image file").unwrap();
        let error = resolve_against(Some("execd"), parsed, rootfs.path(), &[]).unwrap_err();
        assert!(matches!(error, InjectionError::TargetOccupied { .. }));

        // A symlink at the target is occupied too; use a fresh rootfs so the
        // previous file is not in the way.
        let (fresh, source) = staged_rootfs();
        let fresh_catalogue = catalogue_with_source(&source);
        std::fs::create_dir_all(fresh.path().join("opt/opensandbox")).unwrap();
        std::os::unix::fs::symlink("/elsewhere", fresh.path().join("opt/opensandbox/execd"))
            .unwrap();
        let error =
            resolve_against(Some("execd"), &fresh_catalogue, fresh.path(), &[]).unwrap_err();
        assert!(matches!(error, InjectionError::TargetOccupied { .. }));

        // Mount-claimed targets reject before any filesystem check.
        let (mounted, source) = staged_rootfs();
        let mounted_catalogue = catalogue_with_source(&source);
        let mounts = vec![MountPlan {
            destination: PathBuf::from("/opt/opensandbox/execd"),
            fs_type: "bind".into(),
            source: Some("/host".into()),
            options: vec![],
            from_request: true,
        }];
        let error = resolve_against(Some("execd"), &mounted_catalogue, mounted.path(), &mounts)
            .unwrap_err();
        assert!(matches!(error, InjectionError::TargetMounted { .. }));
    }

    #[test]
    fn missing_or_nonregular_sources_are_rejected() {
        let rootfs = tempfile::tempdir().unwrap();
        let missing = catalogue_with_source(Path::new("/nonexistent-pvisor-payload"));
        let error = resolve_against(Some("execd"), &missing, rootfs.path(), &[]).unwrap_err();
        assert!(matches!(error, InjectionError::Source { .. }));

        let dir = tempfile::tempdir().unwrap();
        let directory_source = catalogue_with_source(dir.path());
        let error =
            resolve_against(Some("execd"), &directory_source, rootfs.path(), &[]).unwrap_err();
        assert!(matches!(error, InjectionError::Source { .. }));
    }

    #[test]
    fn resolution_applies_argv_default_and_keeps_env() {
        let (rootfs, source) = staged_rootfs();
        let json = format!(
            r#"{{"payloads":{{
                "execd":{{"source":{s},"target":"/opt/opensandbox/execd","env":{{"P":"1"}}}},
                "tool":{{"source":{s},"target":"/usr/local/bin/tool","argv":["/usr/local/bin/tool","--serve"]}}
            }}}}"#,
            s = serde_json::to_string(&source).unwrap()
        );
        let parsed = catalogue(&json);
        let plans = resolve_against(Some("execd,tool"), &parsed, rootfs.path(), &[]).unwrap();
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].argv, vec!["/opt/opensandbox/execd"]);
        assert_eq!(plans[0].env.get("P").map(String::as_str), Some("1"));
        assert_eq!(plans[1].argv, vec!["/usr/local/bin/tool", "--serve"]);
    }

    #[test]
    fn bind_plans_are_readonly_executable_binds() {
        let plan = InjectionPlan {
            name: "execd".into(),
            source: PathBuf::from("/opt/pvisor/execd"),
            target: PathBuf::from("/opt/opensandbox/execd"),
            argv: vec!["/opt/opensandbox/execd".into()],
            env: BTreeMap::new(),
            run: true,
            bridge: None,
            share: None,
            staged: false,
        };
        let mounts = bind_mount_plans(std::slice::from_ref(&plan));
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].fs_type, "bind");
        assert_eq!(
            mounts[0].destination,
            PathBuf::from("/opt/opensandbox/execd")
        );
        assert_eq!(
            mounts[0].options,
            vec!["ro".to_string(), "nosuid".to_string(), "nodev".to_string()]
        );
    }

    #[test]
    fn merged_env_overlays_payload_on_workload_environment() {
        let base = vec![
            "PATH=/usr/bin".to_string(),
            "EXECD_PORT=1".to_string(),
            "MALFORMED".to_string(),
        ];
        let overlay = BTreeMap::from([("EXECD_PORT".to_string(), "44772".to_string())]);
        let merged = merged_env(&base, &overlay);
        assert!(merged.contains(&"PATH=/usr/bin".to_string()));
        assert!(merged.contains(&"EXECD_PORT=44772".to_string()));
        assert!(!merged.contains(&"EXECD_PORT=1".to_string()));
        assert!(!merged.iter().any(|entry| entry == "MALFORMED"));
    }

    #[test]
    fn stage_only_and_bridge_payloads_resolve_with_new_fields() {
        let (rootfs, source) = staged_rootfs();
        let share_dir = source.parent().unwrap();
        let json = format!(
            r#"{{"share_dir":{share_dir},"payloads":{{
                "bootstrap":{{"source":{s},"target":"/opt/opensandbox/bootstrap.sh","run":false,
                             "share":true}},
                "execd":{{"source":{s},"target":"/opt/opensandbox/execd","run":false,
                         "bridge":44772,"share":true}}
            }}}}"#,
            s = serde_json::to_string(&source).unwrap(),
            share_dir = serde_json::to_string(share_dir).unwrap()
        );
        let parsed = catalogue(&json);
        let plans = resolve_against(Some("bootstrap,execd"), &parsed, rootfs.path(), &[]).unwrap();
        assert_eq!(plans.len(), 2);
        assert!(!plans[0].run, "bootstrap stages only");
        assert!(plans[0].bridge.is_none());
        assert_eq!(plans[0].share.as_deref(), Some(share_dir));
        assert!(!plans[1].run);
        assert_eq!(plans[1].bridge, Some(44772));
        assert_eq!(plans[1].share.as_deref(), Some(share_dir));
        // run defaults to true and bridge/share stay optional.
        let plain = catalogue(&format!(
            r#"{{"payloads":{{"p":{{"source":{s},"target":"/opt/p"}}}}}}"#,
            s = serde_json::to_string(&source).unwrap()
        ));
        let plans = resolve_against(Some("p"), &plain, rootfs.path(), &[]).unwrap();
        assert!(plans[0].run);
        assert!(plans[0].bridge.is_none());
        assert!(plans[0].share.is_none());
        // Share without share_dir, or with a source outside it, is rejected.
        let no_dir = catalogue_error(&format!(
            r#"{{"payloads":{{"p":{{"source":{s},"target":"/opt/p","share":true}}}}}}"#,
            s = serde_json::to_string(&source).unwrap()
        ));
        assert!(no_dir.contains("share_dir"), "{no_dir}");
        let outside = catalogue_error(&format!(
            r#"{{"share_dir":"/opt/pvisor/payloads","payloads":{{"p":{{"source":{s},"target":"/opt/p","share":true}}}}}}"#,
            s = serde_json::to_string(&source).unwrap()
        ));
        assert!(outside.contains("inside share_dir"), "{outside}");
        // A zero bridge port is rejected at catalogue parse.
        let bad = catalogue_error(&format!(
            r#"{{"payloads":{{"p":{{"source":{s},"target":"/opt/p","bridge":0}}}}}}"#,
            s = serde_json::to_string(&source).unwrap()
        ));
        assert!(bad.contains("bridge port"), "{bad}");
    }
}
