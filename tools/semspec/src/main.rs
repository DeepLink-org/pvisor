use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use semspec::{
    helpers,
    ledger::{Approval, Ledger},
    model::Verdict,
    project::{Project, atomic_write},
    runner,
};
use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    time::Duration,
};
#[derive(Parser)]
#[command(
    version,
    about = "Human-reviewed black-box semantic preservation specifications"
)]
struct Cli {
    /// Explicit Markdown inputs; repeat, or pass paths after the command. At least one path is required.
    #[arg(long, global = true, value_name = "PATH")]
    spec_dir: Vec<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    Init {
        directory: PathBuf,
    },
    List {
        #[arg(value_name = "PATH")]
        spec: Vec<PathBuf>,
        #[arg(long)]
        domain: Option<String>,
    },
    Show {
        item: String,
        #[arg(value_name = "PATH")]
        spec: Vec<PathBuf>,
    },
    Lint {
        #[arg(value_name = "PATH")]
        spec: Vec<PathBuf>,
    },
    Review {
        #[arg(value_name = "PATH")]
        spec: Vec<PathBuf>,
        #[arg(long)]
        strict: bool,
    },
    Run {
        /// Extract and run cases from a Markdown file or directory.
        #[arg(value_name = "PATH")]
        spec: Vec<PathBuf>,
        #[arg(long, value_delimiter = ',')]
        case: Vec<String>,
        #[arg(long)]
        domain: Option<String>,
        #[arg(long, env = "SEMSPEC_SUBJECT_BIN")]
        subject_bin: Option<PathBuf>,
        /// Maximum duration of each case (ms/s/m; defaults to 180s for Markdown).
        #[arg(long, default_value = "180s", value_parser = semspec::parse::parse_timeout)]
        timeout: Duration,
        #[arg(long)]
        keep: bool,
        #[arg(long)]
        require_reviewed: bool,
        /// Require every selected case to PASS; SKIP and XFAIL fail this gate.
        #[arg(long)]
        require_pass: bool,
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
    Helper {
        #[command(subcommand)]
        command: Helper,
    },
}
#[derive(Subcommand)]
enum Helper {
    /// Print Markdown preparation blocks for manual execution or fixture checks.
    Setup {
        file: PathBuf,
    },
    TreeState {
        directory: PathBuf,
    },
    JsonGet {
        file: PathBuf,
        pointer: String,
    },
    Diff {
        a: PathBuf,
        b: PathBuf,
    },
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
                Helper::Setup { file } => {
                    print!(
                        "{}",
                        semspec::parse::parse_setup(&fs::read_to_string(file)?)?
                    );
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
        Commands::Init { directory } => {
            init(&directory)?;
            return Ok(0);
        }
        Commands::Approve { .. } => {
            require_terminal(io::stdin().is_terminal(), io::stdout().is_terminal())?;
        }
        _ => {}
    }
    let mut inputs = cli.spec_dir;
    match &cli.command {
        Commands::Run { spec, .. }
        | Commands::List { spec, .. }
        | Commands::Lint { spec }
        | Commands::Show { spec, .. }
        | Commands::Review { spec, .. } => inputs.extend(spec.iter().cloned()),
        _ => {}
    }
    ensure!(
        !inputs.is_empty(),
        "specify at least one Markdown file or directory"
    );
    let mut project = Project::load(&inputs)?;
    match cli.command {
        Commands::List { domain, .. } => {
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
        Commands::Show { item, .. } => {
            show(&project, &item)?;
            Ok(0)
        }
        Commands::Lint { .. } => {
            println!(
                "{} cases, {} preparation documents: valid",
                project.cases.len(),
                project.preparation.len()
            );
            Ok(0)
        }
        Commands::Review { strict, .. } => {
            let mut pending = false;
            for id in project.items() {
                let item = project.item(&id)?;
                pending |= !item.review.reviewed();
                println!("{id:24} {:10} {}", item.review.label(), item.digest);
            }
            Ok(i32::from(strict && pending))
        }
        Commands::Run {
            spec: _,
            case,
            domain,
            subject_bin,
            timeout,
            keep,
            require_reviewed,
            require_pass,
            jobs,
            format,
            output,
        } => {
            ensure!(jobs == 1, "v0.1 is serial; --jobs > 1 requires v0.2");
            let cases = project.select(&case, domain.as_deref())?;
            let expected: Vec<_> = cases.iter().map(|case| case.id.as_str()).collect();
            let output = output.map(|path| -> Result<PathBuf> {
                let path = absolute(&path)?;
                if path.exists() {
                    let target = path.canonicalize()?;
                    ensure!(target != project.root.join("REVIEWED.toml")
                        && !project.cases.iter().any(|case| project.root.join(&case.file) == target)
                        && !project.preparation.keys().any(|name| project.root.join(name) == target),
                        "report output must not overwrite specifications, preparation or the review ledger");
                    fs::remove_file(&path)?;
                }
                fs::create_dir_all(path.parent().unwrap())?;
                Ok(path)
            }).transpose()?;
            let report = runner::run(
                &project,
                &cases,
                &runner::Options {
                    timeout,
                    subject: subject_bin,
                    keep,
                },
            )?;
            let code = report.exit_code(&expected, require_reviewed, require_pass);
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
                    text.push_str(&format!("preparation {name} [{}]\n", review.label()));
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
                atomic_write(&path, text.as_bytes())?;
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
                show(&project, &id)?;
                println!("Digest: {}", item.digest);
                if !confirm(&id, &reviewer)? {
                    continue;
                }
                edit_ledger(&mut project, |ledger| {
                    ledger.approve(Approval {
                        item: id.clone(),
                        digest: item.digest.clone(),
                        reviewer: reviewer.clone(),
                        date: today(),
                        signature: None,
                    })
                })?;
            }
            Ok(0)
        }
        Commands::Helper { .. } | Commands::Init { .. } => unreachable!(),
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
        "approve requires an interactive terminal and human confirmation"
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
fn edit_ledger(project: &mut Project, edit: impl FnOnce(&mut Ledger)) -> Result<()> {
    let path = project.root.join("REVIEWED.toml");
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
    atomic_write(&path, toml::to_string_pretty(&ledger)?.as_bytes())?;
    project.ledger = ledger;
    Ok(())
}
fn init(directory: &Path) -> Result<()> {
    let directory = absolute(directory)?;
    ensure!(
        !directory.join("example.md").exists() && !directory.join("index.md").exists(),
        "init refuses to overwrite existing files"
    );
    fs::create_dir_all(&directory)?;
    for (name, text) in [
        ("example.md", include_str!("../templates/example.md")),
        ("index.md", include_str!("../templates/index.md")),
    ] {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(name))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    println!(
        "Initialized unreviewed Markdown cases at {}",
        directory.display()
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
