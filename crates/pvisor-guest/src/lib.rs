//! Launch contract shared by the host executors and the Linux guest.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::io;
use std::path::PathBuf;
use std::process::Command;

pub const CONFIG_PATH: &str = "/.pvisor-guest.json";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestConfig {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub workspace: Option<PathBuf>,
    /// Linux RLIMIT names, with soft and hard limits in native units.
    #[serde(default)]
    pub limits: BTreeMap<String, (u64, u64)>,
    #[serde(default)]
    pub network: Option<NetworkConfig>,
    #[serde(default)]
    pub agent: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub address: [u8; 4],
    pub gateway: [u8; 4],
}

impl GuestConfig {
    pub fn command(&self) -> io::Result<Command> {
        let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
        if self.argv.first().is_none_or(|program| program.is_empty()) {
            return Err(invalid("guest argv needs a program"));
        }
        if !self.cwd.is_absolute() || self.workspace.as_ref().is_some_and(|p| !p.is_absolute()) {
            return Err(invalid("guest paths must be absolute"));
        }
        for path in std::iter::once(&self.cwd).chain(self.workspace.iter()) {
            CString::new(path.as_os_str().as_encoded_bytes())
                .map_err(|_| invalid("guest paths contain NUL"))?;
        }
        if self
            .agent
            .as_ref()
            .is_some_and(|argv| argv.first().is_none_or(|p| p.is_empty()))
        {
            return Err(invalid("guest agent argv needs a program"));
        }
        for word in self
            .argv
            .iter()
            .chain(self.env.values())
            .chain(self.agent.iter().flatten())
        {
            CString::new(word.as_str()).map_err(|_| invalid("guest arguments contain NUL"))?;
        }
        if self
            .env
            .keys()
            .any(|key| key.is_empty() || key.contains(['=', '\0']))
        {
            return Err(invalid("invalid guest environment name"));
        }
        if self.limits.values().any(|(soft, hard)| soft > hard) {
            return Err(invalid("guest soft limit exceeds hard limit"));
        }
        let mut command = Command::new(&self.argv[0]);
        command
            .args(&self.argv[1..])
            .env_clear()
            .envs(&self.env)
            .current_dir(&self.cwd);
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_contract_preserves_values_and_rejects_invalid_input() {
        let config = GuestConfig {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "test -z \"${HOME+x}\" || exit 41; printf '%s' \"$COMPLEX\"; exit 7".into(),
            ],
            env: BTreeMap::from([("COMPLEX".into(), "space ' quote \"\nnewline".into())]),
            cwd: std::env::temp_dir(),
            ..Default::default()
        };
        let bytes = serde_json::to_vec(&config).unwrap();
        let mut decoded: GuestConfig = serde_json::from_slice(&bytes).unwrap();
        let output = decoded.command().unwrap().output().unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            config.env["COMPLEX"]
        );
        decoded.argv.push("bad\0argument".into());
        assert!(decoded.command().is_err());
        decoded.argv.clear();
        assert!(decoded.command().is_err());
    }
}
