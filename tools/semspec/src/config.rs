use anyhow::{Result, ensure};
use serde::Deserialize;
use std::{collections::BTreeMap, path::PathBuf, time::Duration};
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub project: ProjectConfig,
    pub subject: SubjectConfig,
    pub platforms: BTreeMap<String, Platform>,
    #[serde(default)]
    pub signing: Signing,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub name: String,
    pub spec_dirs: Vec<PathBuf>,
    pub ledger: PathBuf,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectConfig {
    pub bin: PathBuf,
    pub language: String,
    pub vocab: Vec<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub timeout: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Platform {
    pub os: String,
}
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signing {
    #[serde(default)]
    pub required: bool,
    pub allowed_signers: Option<PathBuf>,
}
impl Config {
    pub fn parse(source: &str) -> Result<Self> {
        let config: Self = toml::from_str(source)?;
        ensure!(
            !config.project.name.is_empty() && !config.project.spec_dirs.is_empty(),
            "project name/spec_dirs required"
        );
        ensure!(config.subject.language == "bash", "v0.1 supports only bash");
        ensure!(
            !config.signing.required,
            "required SSH verification needs semspec v0.2"
        );
        ensure!(
            config.subject.env.keys().all(|k| !k.starts_with("SEMSPEC_")
                && !["CASE_ROOT", "WS", "SUBJECT_BIN"].contains(&k.as_str())),
            "reserved runner environment variable"
        );
        config.timeout()?;
        ensure!(!config.platforms.is_empty(), "platforms are required");
        for (name, platform) in &config.platforms {
            ensure!(
                !name.is_empty() && !platform.os.is_empty(),
                "invalid platform"
            );
        }
        Ok(config)
    }
    pub fn timeout(&self) -> Result<Duration> {
        let text = &self.subject.timeout;
        let (value, multiplier) = if let Some(v) = text.strip_suffix("ms") {
            (v, 1)
        } else if let Some(v) = text.strip_suffix('s') {
            (v, 1000)
        } else if let Some(v) = text.strip_suffix('m') {
            (v, 60_000)
        } else {
            anyhow::bail!("timeout requires ms/s/m suffix");
        };
        let millis = value
            .parse::<u64>()?
            .checked_mul(multiplier)
            .ok_or_else(|| anyhow::anyhow!("timeout overflow"))?;
        ensure!(millis > 0, "timeout must be positive");
        Ok(Duration::from_millis(millis))
    }
    pub fn platform(&self) -> Result<String> {
        let found: Vec<_> = self
            .platforms
            .iter()
            .filter(|(_, p)| p.os == std::env::consts::OS)
            .collect();
        ensure!(
            found.len() == 1,
            "current OS must match exactly one configured platform"
        );
        Ok(found[0].0.clone())
    }
}
