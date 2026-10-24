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
    /// Stable review root; otherwise preserve legacy nearest-common-directory behavior.
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Transform executable Markdown; execution is explicit and needs no report file.
    Filter {
        input: Option<PathBuf>,
        #[arg(long, conflicts_with = "input")]
        source: Option<PathBuf>,
        #[arg(long)]
        stdin: bool,
        #[arg(long, conflicts_with = "results")]
        execute: bool,
        #[arg(long)]
        results: Option<PathBuf>,
        #[arg(long, requires = "execute")]
        artifacts: Option<PathBuf>,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long, default_value = "markdown", value_parser = ["markdown", "json"])]
        format: String,
        #[arg(long)]
        strict: bool,
        #[arg(long)]
        best_effort: bool,
        #[arg(long, env = "SEMSPEC_SUBJECT_BIN")]
        subject_bin: Option<PathBuf>,
        #[arg(long, default_value = "180s", value_parser = semspec::parse::parse_timeout)]
        timeout: Duration,
        #[arg(long)]
        require_reviewed: bool,
    },
    /// Execute a JSON or inline Markdown table; write INPUT.render.json and INPUT.bench.json.
    Bench {
        input: PathBuf,
        /// Working directory for commands; defaults to the template directory.
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Replace standalone {{ table("file.render.json") }} placeholders; print Markdown.
    Render {
        input: PathBuf,
        /// Optional output file; otherwise write to stdout. The source cannot be overwritten.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Read Markdown from stdin, using INPUT only as the source path for relative tables.
        #[arg(long)]
        stdin: bool,
        /// Continue past table errors with visible warnings; intended for live previews.
        #[arg(long)]
        best_effort: bool,
    },
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
    if let Err(error) = runner::install_signal_handlers() {
        eprintln!("semspec: {error:#}");
        std::process::exit(3);
    }
    match dispatch(cli) {
        Ok(code) => std::process::exit(if runner::interruption() != 0 {
            128 + runner::interruption()
        } else {
            code
        }),
        Err(error) => {
            eprintln!("semspec: {error:#}");
            std::process::exit(if runner::interruption() != 0 {
                128 + runner::interruption()
            } else {
                2
            });
        }
    }
}
fn dispatch(cli: Cli) -> Result<i32> {
    match cli.command {
        Commands::Filter {
            input,
            source,
            stdin,
            execute,
            results,
            artifacts,
            output,
            format,
            strict,
            best_effort,
            subject_bin,
            timeout,
            require_reviewed,
        } => {
            ensure!(
                cli.spec_dir.is_empty(),
                "filter accepts one explicit Markdown source"
            );
            let input = input
                .or(source)
                .ok_or_else(|| anyhow::anyhow!("filter requires INPUT or --source PATH"))?
                .canonicalize()?;
            let text = if stdin {
                std::io::read_to_string(io::stdin())?
            } else {
                fs::read_to_string(&input)?
            };
            let loaded: Option<semspec::model::RunReport> = results
                .as_ref()
                .map(|path| -> Result<_> {
                    let path = if path.is_dir() {
                        path.join("report.json")
                    } else {
                        path.clone()
                    };
                    Ok(serde_json::from_slice(&fs::read(path)?)?)
                })
                .transpose()?;
            let has_cases = !semspec::parse::parse_document(&input, &text)?.is_empty();
            let experiments = semspec::experiment::cells(&text)?;
            let tables = semspec::table::cells(&text)?;
            ensure!(
                !execute || has_cases || !experiments.is_empty() || !tables.is_empty(),
                "no executable cells in document"
            );
            ensure!(
                !execute || text == fs::read_to_string(&input)?,
                "execution requires the canonical source bytes"
            );
            let root = cli
                .root
                .as_deref()
                .or_else(|| loaded.as_ref().and_then(|r| r.root.as_deref()));
            let project = has_cases
                .then(|| Project::load_at(&[&input], root))
                .transpose()?;
            if let Some(output) = &output {
                protect_output(output, &input, project.as_ref())?;
            }
            if let Some(path) = &artifacts {
                fs::create_dir(path)?;
                if let Some(output) = &output {
                    ensure!(
                        resolved_target(&absolute(output)?)?
                            != resolved_target(&path.canonicalize()?.join("report.json"))?,
                        "filter output must not overwrite its artifact report"
                    );
                }
            }
            let mut code = 0;
            let generated = if execute {
                if let Some(output) = &output
                    && output.exists()
                {
                    fs::remove_file(output)?;
                }
                let mut report = if let Some(project) = project.as_ref() {
                    let cases = project.select(&[], None)?;
                    let report = runner::run(
                        project,
                        &cases,
                        &runner::Options {
                            timeout,
                            subject: subject_bin,
                            keep: artifacts.is_some(),
                            artifact_root: artifacts
                                .as_ref()
                                .map(|p| p.canonicalize())
                                .transpose()?,
                        },
                    )?;
                    code = report.exit_code(
                        &cases.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
                        require_reviewed,
                        true,
                    );
                    report
                } else {
                    ensure!(
                        !require_reviewed,
                        "benchmark observations do not have approval records"
                    );
                    semspec::model::RunReport {
                        engine_semantics: semspec::seal::ENGINE_SEMANTICS.into(),
                        engine_review: semspec::model::ReviewState::Unreviewed,
                        vocab_review: vec![],
                        platform: std::env::consts::OS.into(),
                        results: vec![],
                        benchmarks: vec![],
                        root: None,
                    }
                };
                for cell in &experiments {
                    if runner::interruption() != 0 {
                        break;
                    }
                    let outcome = semspec::experiment::run(cell, &input)?;
                    if outcome.data["status"] != "PASS" {
                        code = code.max(1);
                    }
                    report.benchmarks.push(outcome);
                }
                for cell in &tables {
                    if runner::interruption() != 0 {
                        break;
                    }
                    let outcome = semspec::table::run(cell, &input, artifacts.as_deref())?;
                    if outcome.data["status"] != "PASS" {
                        code = code.max(1);
                    }
                    report.benchmarks.push(outcome);
                }
                if let Some(path) = &artifacts {
                    atomic_write(
                        &path.join("report.json"),
                        &serde_json::to_vec_pretty(&report)?,
                    )?;
                }
                Some(report)
            } else {
                None
            };
            let report = generated.as_ref().or(loaded.as_ref());
            let mut document = semspec::filter::render(
                &text,
                &input,
                project.as_ref(),
                report,
                strict,
                best_effort,
            )?;
            if let Some(path) = results {
                document.dependencies.push(if path.is_dir() {
                    path.join("report.json").canonicalize()?
                } else {
                    path.canonicalize()?
                });
            }
            if let Some(output) = &output {
                let target = resolved_target(&absolute(output)?)?;
                for dependency in &document.dependencies {
                    ensure!(
                        target != resolved_target(dependency)?,
                        "filter output must not overwrite dependency {}",
                        dependency.display()
                    );
                }
            }
            let text = if format == "json" {
                format!("{}\n", serde_json::to_string_pretty(&document)?)
            } else {
                document.markdown
            };
            if let Some(output) = output {
                atomic_write(&absolute(&output)?, text.as_bytes())?;
            } else {
                print!("{text}");
            }
            return Ok(code);
        }
        Commands::Bench { input, cwd } => {
            ensure!(cli.spec_dir.is_empty(), "bench does not accept --spec-dir");
            let output = semspec::table::bench_at(&input, cwd.as_deref())?;
            println!("{}", output.display());
            return Ok(0);
        }
        Commands::Render {
            input,
            output,
            stdin,
            best_effort,
        } => {
            ensure!(cli.spec_dir.is_empty(), "render does not accept --spec-dir");
            let source = if stdin {
                std::io::read_to_string(io::stdin())?
            } else {
                fs::read_to_string(&input)?
            };
            let text = semspec::table::render(&source, &input, best_effort)?;
            if let Some(output) = output {
                let output = absolute(&output)?;
                ensure!(
                    output != absolute(&input)?
                        && (!output.exists()
                            || !input.exists()
                            || output.canonicalize()? != input.canonicalize()?),
                    "render output must not overwrite its Markdown source"
                );
                atomic_write(&output, text.as_bytes())?;
            } else {
                print!("{text}");
            }
            return Ok(0);
        }
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
    let mut project = Project::load_at(&inputs, cli.root.as_deref())?;
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
                protect_output(&path, &project.root.join("REVIEWED.toml"), Some(&project))?;
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
                    artifact_root: None,
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
        Commands::Helper { .. }
        | Commands::Filter { .. }
        | Commands::Init { .. }
        | Commands::Bench { .. }
        | Commands::Render { .. } => unreachable!(),
    }
}
fn protect_output(output: &Path, source: &Path, project: Option<&Project>) -> Result<()> {
    let output = absolute(output)?;
    let target = resolved_target(&output)?;
    ensure!(
        target != source,
        "filter output must not overwrite its Markdown source"
    );
    if let Some(project) = project {
        ensure!(
            target != project.root.join("REVIEWED.toml")
                && !project
                    .cases
                    .iter()
                    .any(|c| project.root.join(&c.file) == target)
                && !project
                    .preparation
                    .keys()
                    .any(|p| project.root.join(p) == target),
            "output must not overwrite specifications, preparation or the review ledger"
        );
    }
    Ok(())
}
fn resolved_target(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("output requires a parent"))?;
    Ok(resolved_target(parent)?.join(
        path.file_name()
            .ok_or_else(|| anyhow::anyhow!("invalid output path"))?,
    ))
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
