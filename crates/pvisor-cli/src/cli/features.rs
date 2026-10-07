//! Runtime feature listing frontend.
use pvisor::features::{Feature, FeatureSettings, REGISTRY};

#[derive(Debug, clap::Args)]
pub(crate) struct FeatureArgs {
    /// Optional synonym for the default listing action.
    #[arg(value_parser = ["list"])]
    action: Option<String>,
    #[arg(long)]
    json: bool,
}

impl FeatureArgs {
    pub(crate) fn print(&self, enabled: &[Feature]) -> anyhow::Result<()> {
        let mut settings = FeatureSettings::default();
        for feature in enabled {
            settings.enable(*feature);
        }
        let rows: Vec<_> = REGISTRY
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "name": entry.name, "stage": entry.stage, "default": entry.default,
                    "enabled": settings.enabled(entry.feature), "description": entry.description,
                })
            })
            .collect();
        if self.json {
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            println!("name\tstage\tdefault\tenabled\tdescription");
            for entry in REGISTRY {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    entry.name,
                    entry.stage,
                    entry.default,
                    settings.enabled(entry.feature),
                    entry.description
                );
            }
        }
        Ok(())
    }
}
