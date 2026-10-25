//! Files and result hand-off for a pVisor delegated through an OCI container or libkrun VM.

use crate::util::write_private_json;
use pvisor_core::{AttemptId, RunInvocation, RunResult, RunSpec};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

pub(crate) const SPEC_FILENAME: &str = "run-spec.json";
pub(crate) const RESULT_FILENAME: &str = "run-result.json";
const MAX_RESULT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct DelegatedRunOutput {
    pub result: RunResult,
    pub agentctl: crate::AgentCtlSnapshot,
}

pub(crate) struct DelegatedRunFiles {
    _temporary: tempfile::TempDir,
    pub(crate) spec_path: PathBuf,
    pub(crate) result_path: PathBuf,
}

impl DelegatedRunFiles {
    /// Create delegated files, optionally capturing the injected pVisor's output.
    /// The outer transport owns the real terminal; inheriting it in the nested
    /// process makes rootless OCI runs attempt tty process-group operations.
    pub(crate) fn new_with_stdio(spec: &RunSpec, capture: bool) -> anyhow::Result<Self> {
        let temporary = tempfile::Builder::new()
            .prefix("pvisor-delegated-")
            .tempdir()?;
        let spec_path = temporary.path().join(SPEC_FILENAME);
        let output_dir = temporary.path().join("output");
        std::fs::create_dir(&output_dir)?;
        let result_path = output_dir.join(RESULT_FILENAME);
        let mut delegated = spec.clone();
        delegated.metadata.remove("pvisor.executor");
        let RunInvocation::Process(process) = &mut delegated.invocation;
        process
            .env
            .retain(|key, _| !key.starts_with("PVISOR_AGENTCTL_"));
        if capture {
            // Captured stdin is unsupported. Null stdin also
            // prevents the nested host executor from attempting tty control.
            process.stdin = pvisor_core::StdioMode::Null;
            process.stdout = pvisor_core::StdioMode::Capture;
            process.stderr = pvisor_core::StdioMode::Capture;
        }
        write_private_json(&spec_path, &delegated)?;
        Ok(Self {
            _temporary: temporary,
            spec_path,
            result_path,
        })
    }

    pub(crate) fn read_result(
        &self,
        run_id: &pvisor_core::RunId,
        attempt_id: &AttemptId,
    ) -> anyhow::Result<DelegatedRunOutput> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&self.result_path)?;
        anyhow::ensure!(
            file.metadata()?.is_file(),
            "delegated result must be a regular file"
        );
        let mut bytes = Vec::new();
        file.take(MAX_RESULT_BYTES + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_RESULT_BYTES,
            "delegated result exceeds size limit"
        );
        let mut output: DelegatedRunOutput = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            output.result.run_id == *run_id
                && output.agentctl.run_id == run_id.as_str()
                && output.agentctl.attempt_id == output.result.attempt_id.as_str(),
            "delegated result identity mismatch"
        );
        output.result.attempt_id = attempt_id.clone();
        output.agentctl.attempt_id = attempt_id.to_string();
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegated_result_uses_the_owning_local_attempt_identity() {
        let spec = RunSpec::process("outer-run", "agent", "/bin/sh");
        let files = DelegatedRunFiles::new_with_stdio(&spec, false).unwrap();
        let output = serde_json::json!({
            "result": {"run_id":"outer-run", "attempt_id":"inner-attempt", "state":"completed", "started_at_unix_ms":1, "finished_at_unix_ms":2},
            "agentctl": {"run_id":"outer-run", "attempt_id":"inner-attempt", "directive":crate::AgentDirective::Continue, "clients":[]}
        });
        write_private_json(&files.result_path, &output).unwrap();
        let attempt_id = AttemptId::new("outer-attempt");
        let output = files.read_result(&spec.run_id, &attempt_id).unwrap();
        assert_eq!(output.result.run_id, spec.run_id);
        assert_eq!(output.result.attempt_id, attempt_id);
        assert_eq!(output.agentctl.run_id, "outer-run");
        assert_eq!(output.agentctl.attempt_id, "outer-attempt");
    }

    #[test]
    fn delegated_results_reject_foreign_identity_symlinks_and_oversized_files() {
        let spec = RunSpec::process("outer-run", "agent", "/bin/sh");
        let files = DelegatedRunFiles::new_with_stdio(&spec, false).unwrap();
        let attempt = AttemptId::new("outer-attempt");
        let foreign = serde_json::json!({
            "result": {"run_id":"foreign", "attempt_id":"inner", "state":"completed", "started_at_unix_ms":1, "finished_at_unix_ms":2},
            "agentctl": {"run_id":"foreign", "attempt_id":"inner", "directive":crate::AgentDirective::Continue, "clients":[]}
        });
        write_private_json(&files.result_path, &foreign).unwrap();
        assert!(
            files
                .read_result(&spec.run_id, &attempt)
                .unwrap_err()
                .to_string()
                .contains("identity")
        );
        std::fs::remove_file(&files.result_path).unwrap();
        std::os::unix::fs::symlink(&files.spec_path, &files.result_path).unwrap();
        assert!(files.read_result(&spec.run_id, &attempt).is_err());
        std::fs::remove_file(&files.result_path).unwrap();
        std::fs::File::create(&files.result_path)
            .unwrap()
            .set_len(MAX_RESULT_BYTES + 1)
            .unwrap();
        assert!(
            files
                .read_result(&spec.run_id, &attempt)
                .unwrap_err()
                .to_string()
                .contains("size limit")
        );
    }

    #[test]
    fn delegated_spec_drops_host_agentctl() {
        let mut spec = RunSpec::process("run-one", "agent", "true");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process
            .env
            .insert("PVISOR_AGENTCTL_ENDPOINT".into(), "/tmp/host.sock".into());
        process.env.insert("KEEP".into(), "yes".into());
        let files = DelegatedRunFiles::new_with_stdio(&spec, false).unwrap();
        let delegated: RunSpec =
            serde_json::from_slice(&std::fs::read(&files.spec_path).unwrap()).unwrap();
        let RunInvocation::Process(process) = delegated.invocation;
        assert!(!process.env.contains_key("PVISOR_AGENTCTL_ENDPOINT"));
        assert_eq!(process.env.get("KEEP").map(String::as_str), Some("yes"));
    }
}
