//! Validate documentation TOML with the same deserializer used by `pvisor run`.
//! No executor, filesystem mount, network listener, or VM is started.
use std::path::Path;

fn main() -> anyhow::Result<()> {
    let config = match std::env::args().nth(1) {
        Some(path) => pvisor::RunConfig::from_file(Path::new(&path))?,
        None => pvisor::RunConfig::default(),
    };
    println!("{}", serde_json::to_string_pretty(&config)?);
    Ok(())
}
