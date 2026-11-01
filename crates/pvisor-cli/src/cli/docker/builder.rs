//! Docker-compatible build-cache pruning, separate from image removal.
use super::*;

#[derive(Debug, Args)]
pub(in crate::cli) struct BuilderArgs {
    #[command(subcommand)]
    command: BuilderCommand,
}

#[derive(Debug, Subcommand)]
enum BuilderCommand {
    /// Remove unused build cache, waiting for active builds to finish.
    Prune {
        /// Also remove cache checkpoints matching published images.
        #[arg(short, long)]
        all: bool,
        /// Do not prompt for confirmation.
        #[arg(short, long)]
        force: bool,
        /// Remove cache unused for this long, e.g. until=24h (repeatable).
        #[arg(long = "filter")]
        filters: Vec<String>,
        /// Amount of cache to keep, e.g. 512MB or 2GB.
        #[arg(long, value_parser = storage)]
        keep_storage: Option<u64>,
    },
}

fn age(value: &str) -> anyhow::Result<Duration> {
    let (digits, multiplier) = if let Some(v) = value.strip_suffix("ms") {
        (v, 1)
    } else if let Some(v) = value.strip_suffix('s') {
        (v, 1000)
    } else if let Some(v) = value.strip_suffix('m') {
        (v, 60_000)
    } else if let Some(v) = value.strip_suffix('h') {
        (v, 3_600_000)
    } else {
        bail!("until requires a duration in ms, s, m or h");
    };
    Ok(Duration::from_millis(
        digits
            .parse::<u64>()?
            .checked_mul(multiplier)
            .context("until duration overflow")?,
    ))
}

fn storage(value: &str) -> Result<u64, String> {
    let upper = value.to_ascii_uppercase();
    let (number, multiplier) = ["GB", "MB", "KB", "B"]
        .into_iter()
        .zip([1_000_000_000u64, 1_000_000, 1000, 1])
        .find_map(|(suffix, multiplier)| upper.strip_suffix(suffix).map(|v| (v, multiplier)))
        .unwrap_or((&upper, 1));
    number
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
        .ok_or_else(|| "keep-storage requires nonnegative integer bytes or KB/MB/GB".into())
}

pub(super) fn run(args: BuilderArgs) -> anyhow::Result<i32> {
    let BuilderCommand::Prune {
        all,
        force,
        filters,
        keep_storage,
    } = args.command;
    let mut until = None;
    for filter in filters {
        let value = filter
            .strip_prefix("until=")
            .context("only --filter until=DURATION is supported")?;
        let duration = age(value)?;
        until = Some(until.map_or(duration, |previous: Duration| previous.max(duration)));
    }
    if !force {
        use std::io::IsTerminal;
        ensure!(
            io::stdin().is_terminal(),
            "builder prune requires --force without an interactive terminal"
        );
        eprintln!(
            "WARNING! This will remove unused build cache. Images and containers are not removed."
        );
        eprint!("Are you sure you want to continue? [y/N] ");
        io::stderr().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            return Ok(0);
        }
    }
    let report = docker::build::prune(
        &Store::discover()?,
        &docker::build::PruneOptions {
            all,
            until,
            keep_storage,
        },
    )?;
    println!(
        "Deleted build cache records: {}\nTotal reclaimed space: {}B",
        report.records, report.bytes
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_values_are_bounded_and_explicit() {
        assert_eq!(age("24h").unwrap(), Duration::from_secs(86400));
        assert_eq!(storage("2GB").unwrap(), 2_000_000_000);
        for value in ["-1h", "24", "18446744073709551615h"] {
            assert!(age(value).is_err());
        }
        for value in ["-1", "NaN", "18446744073709551615GB"] {
            assert!(storage(value).is_err());
        }
    }
}
