use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use semspec::{
    helpers,
    ledger::{Approval, Ledger, Revocation},
    model::Verdict,
    project::{Project, atomic_write},
    runner,
};
use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};
#[derive(Parser)]
#[command(
    version,
    about = "Human-reviewed black-box semantic preservation specifications"
)]
struct Cli {
    #[arg(long, global = true, default_value = "semspec.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    Init,
    List {
        #[arg(long)]
        domain: Option<String>,
    },
    Show {
        item: String,
    },
    Lint,
    Review {
        #[arg(long)]
        strict: bool,
    },
    Diff {
        item: String,
    },
    Run {
        /// Run cases from this Markdown file in the configured spec_dirs.
        spec: Option<PathBuf>,
        #[arg(long, value_delimiter = ',')]
        case: Vec<String>,
        #[arg(long)]
        domain: Option<String>,
        #[arg(long, env = "SEMSPEC_SUBJECT_BIN")]
        subject_bin: Option<PathBuf>,
        #[arg(long)]
        keep: bool,
        #[arg(long)]
        require_reviewed: bool,
        #[arg(long, default_value = "1")]
        jobs: usize,
        #[arg(long,default_value="human",value_parser=["human","json"])]
        format: String,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Approve {
        #[arg(required = true)]
        items: Vec<String>,
        #[arg(long)]
        reviewer: String,
        #[arg(long)]
        sign: Option<PathBuf>,
    },
    Revoke {
        #[arg(required = true)]
        items: Vec<String>,
        #[arg(long)]
        reviewer: String,
        #[arg(long)]
        reason: String,
    },
    Helper {
        #[command(subcommand)]
        command: Helper,
    },
}
#[derive(Subcommand)]
enum Helper {
    TreeState { directory: PathBuf },
    JsonGet { file: PathBuf, pointer: String },
    Diff { a: PathBuf, b: PathBuf },
}
fn main() {
    let cli = Cli::parse();
    match dispatch(cli) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("semspec: {error:#}");
            std::process::exit(2);
        }
    }
}
fn dispatch(cli: Cli) -> Result<i32> {
    match cli.command {
        Commands::Helper { command } => {
            return match command {
                Helper::TreeState { directory } => {
                    print!("{}", helpers::tree_state(&directory)?);
                    Ok(0)
                }
                Helper::JsonGet { file, pointer } => {
                    println!("{}", helpers::json_get(&file, &pointer)?);
                    Ok(0)
                }
                Helper::Diff { a, b } => {
                    let (text, code) = helpers::file_diff(&a, &b)?;
                    print!("{text}");
                    Ok(code)
                }
            };
        }
        Commands::Init => {
            init(&cli.config)?;
            return Ok(0);
        }
        Commands::Approve { .. } | Commands::Revoke { .. } => {
            require_terminal(io::stdin().is_terminal(), io::stdout().is_terminal())?;
        }
        _ => {}
    }
    let mut project = Project::load(&cli.config)?;
    match cli.command {
        Commands::List { domain } => {
            if let Some(d) = &domain {
                ensure!(
                    project.cases.iter().any(|c| c.domain == *d),
                    "unknown domain {d}"
                );
            }
            for case in &project.cases {
                if domain.as_ref().is_none_or(|d| case.domain == *d) {
                    println!(
                        "{}  {:10}  {}",
                        case.id,
                        project.item(&case.id)?.review.label(),
                        case.title
                    );
                }
            }
            Ok(0)
        }
        Commands::Show { item } => {
            show(&project, &item)?;
            Ok(0)
        }
        Commands::Lint => {
            println!(
                "{} cases, {} vocabulary files: valid",
                project.cases.len(),
                project.vocab.len()
            );
            Ok(0)
        }
        Commands::Review { strict } => {
            let mut pending = false;
            for id in project.items() {
                let item = project.item(&id)?;
                pending |= !item.review.reviewed();
                println!("{id:24} {:10} {}", item.review.label(), item.digest);
            }
            Ok(i32::from(strict && pending))
        }
        Commands::Diff { item } => {
            show_diff(&project, &item)?;
            Ok(0)
        }
        Commands::Run {
            spec,
            case,
            domain,
            subject_bin,
            keep,
            require_reviewed,
            jobs,
            format,
            output,
        } => {
            ensure!(jobs == 1, "v0.1 is serial; --jobs > 1 requires v0.2");
            let report = runner::run(
                &project,
                &runner::Options {
                    spec,
                    subject: subject_bin,
                    keep,
                    case_ids: case,
                    domain,
                },
            )?;
            let code = report.exit_code(require_reviewed);
            let text = if format == "json" {
                format!("{}\n", serde_json::to_string_pretty(&report)?)
            } else {
                let mut text = format!(
                    "engine {} [{}], platform {}\n",
                    report.engine_semantics,
                    report.engine_review.label(),
                    report.platform
                );
                for (name, review) in &report.vocab_review {
                    text.push_str(&format!("vocabulary {name} [{}]\n", review.label()));
                }
                for result in &report.results {
                    text.push_str(&format!(
                        "{:5} {} [{}] {}ms\n",
                        result.verdict.label(),
                        result.id,
                        result.review.label(),
                        result.duration_ms
                    ));
                    let detail = match &result.verdict {
                        Verdict::Fail { output_tail } => Some(output_tail),
                        Verdict::Error { message } => Some(message),
                        Verdict::Skip { reason, .. } | Verdict::XFail { reason, .. } => {
                            Some(reason)
                        }
                        _ => None,
                    };
                    if let Some(detail) = detail {
                        text.push_str(&format!("  {detail}\n"));
                    }
                    if let Some(path) = &result.workdir {
                        text.push_str(&format!("  kept: {}\n", path.display()));
                    }
                }
                text.push_str(&format!("{} cases; exit {code}\n", report.results.len()));
                text
            };
            if let Some(path) = output {
                atomic_write(&absolute(&path)?, text.as_bytes())?;
            } else {
                print!("{text}");
            }
            Ok(code)
        }
        Commands::Approve {
            items,
            reviewer,
            sign,
        } => {
            ensure!(sign.is_none(), "SSH signing requires v0.2");
            ensure!(!reviewer.trim().is_empty(), "reviewer required");
            for id in items {
                let item = project.item(&id)?;
                if matches!(item.review, semspec::model::ReviewState::Stale { .. }) {
                    show_diff(&project, &id)?;
                } else {
                    show(&project, &id)?;
                }
                println!("Digest: {}", item.digest);
                if !confirm(&id, &reviewer)? {
                    continue;
                }
                edit_ledger(
                    &mut project,
                    |ledger| {
                        ledger.approve(Approval {
                            item: id.clone(),
                            digest: item.digest.clone(),
                            reviewer: reviewer.clone(),
                            date: today(),
                            signature: None,
                        })
                    },
                    Some((&id, &item.text)),
                )?;
            }
            Ok(0)
        }
        Commands::Revoke {
            items,
            reviewer,
            reason,
        } => {
            ensure!(
                !reviewer.trim().is_empty() && !reason.trim().is_empty(),
                "reviewer/reason required"
            );
            for id in items {
                show(&project, &id)?;
                println!("Revoke reason: {reason}");
                if !confirm(&id, &reviewer)? {
                    continue;
                }
                edit_ledger(
                    &mut project,
                    |ledger| {
                        ledger.revoke(Revocation {
                            item: id.clone(),
                            reviewer: reviewer.clone(),
                            date: today(),
                            reason: reason.clone(),
                        })
                    },
                    None,
                )?;
            }
            Ok(0)
        }
        Commands::Helper { .. } | Commands::Init => unreachable!(),
    }
}
fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(std::env::current_dir()?.join(path))
}
fn today() -> String {
    chrono::Local::now().date_naive().to_string()
}
fn require_terminal(input: bool, output: bool) -> Result<()> {
    ensure!(
        input && output,
        "approve/revoke require an interactive terminal and human confirmation"
    );
    Ok(())
}
fn confirm(id: &str, reviewer: &str) -> Result<bool> {
    print!("Type {id} to confirm human review as {reviewer}: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(answer.trim() == id)
}
fn show(project: &Project, id: &str) -> Result<()> {
    let item = project.item(id)?;
    println!(
        "{} [{}] {}\n{}",
        item.id,
        item.review.label(),
        item.digest,
        item.text
    );
    Ok(())
}
fn show_diff(project: &Project, id: &str) -> Result<()> {
    let item = project.item(id)?;
    let path = project.snapshot_path(id)?;
    if path.exists() {
        let before = fs::read_to_string(path)?;
        let diff = helpers::unified_diff(&before, &item.text);
        if diff.is_empty() {
            println!(
                "No text difference; digest/review dependencies may differ: {}",
                item.digest
            );
        } else {
            print!("{diff}");
        }
    } else {
        println!("No approved snapshot; current text:\n{}", item.text);
    }
    Ok(())
}
fn edit_ledger(
    project: &mut Project,
    edit: impl FnOnce(&mut Ledger),
    snapshot: Option<(&str, &str)>,
) -> Result<()> {
    let path = project.root.join(&project.config.project.ledger);
    fs::create_dir_all(path.parent().unwrap())?;
    let lock_path = path.with_extension("toml.lock");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    lock.lock()?;
    let mut ledger = if path.exists() {
        Ledger::parse(&fs::read_to_string(&path)?)?
    } else {
        Ledger::default()
    };
    edit(&mut ledger);
    if let Some((id, text)) = snapshot {
        atomic_write(&project.snapshot_path(id)?, text.as_bytes())?;
    }
    atomic_write(&path, toml::to_string_pretty(&ledger)?.as_bytes())?;
    project.ledger = ledger;
    Ok(())
}
fn init(config: &Path) -> Result<()> {
    let config = absolute(config)?;
    let root = config.parent().context("config needs a directory")?;
    let files = [
        (config.clone(), include_str!("../templates/semspec.toml")),
        (
            root.join("semantics/example.md"),
            include_str!("../templates/example.md"),
        ),
        (
            root.join("semantics/vocab/core.sh"),
            include_str!("../templates/vocab/core.sh"),
        ),
        (
            root.join("semantics/REVIEWED.toml"),
            "# Human-owned review ledger; no approvals yet.\nformat = 1\n",
        ),
    ];
    ensure!(
        files.iter().all(|(p, _)| !p.exists()),
        "init refuses to overwrite existing files"
    );
    for (path, text) in files {
        fs::create_dir_all(path.parent().unwrap())?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    println!(
        "Initialized unreviewed specifications at {}",
        root.display()
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_operations_require_terminal_on_both_sides() {
        assert!(require_terminal(false, true).is_err());
        assert!(require_terminal(true, false).is_err());
        assert!(require_terminal(true, true).is_ok());
    }
}
