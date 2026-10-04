//! Canonical immutable environment identities, independent of cache locations.
use crate::{EnvironmentRecord, EnvironmentTemplate};

pub fn record(template: EnvironmentTemplate) -> anyhow::Result<EnvironmentRecord> {
    template.validate()?;
    let digest = blake3::hash(&serde_json::to_vec(&template)?)
        .to_hex()
        .to_string();
    Ok(EnvironmentRecord { digest, template })
}
pub fn validate(recorded: &EnvironmentRecord) -> anyhow::Result<()> {
    anyhow::ensure!(
        record(recorded.template.clone())? == *recorded,
        "environment digest mismatch"
    );
    Ok(())
}
