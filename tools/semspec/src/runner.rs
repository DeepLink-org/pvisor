use crate::{
    model::{Case, CaseResult, RunReport, Verdict},
    project::Project,
    seal::ENGINE_SEMANTICS,
};
use anyhow::{Context, Result, ensure};
#[cfg(unix)]
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

pub struct Options {
    pub timeout: Duration,
    pub subject: Option<PathBuf>,
    pub keep: bool,
}
pub fn run(project: &Project, cases: &[&Case], options: &Options) -> Result<RunReport> {
    let platform = std::env::consts::OS.to_owned();
    ensure!(
        ["linux", "macos"].contains(&platform.as_str()),
        "Bash checks require Linux or macOS"
    );
    let subject = options.subject.clone();
    let subject = subject
        .map(|path| -> Result<PathBuf> {
            let path = path.canonicalize().context("subject binary unavailable")?;
            ensure!(path.is_file(), "subject must be a file");
            #[cfg(unix)]
            ensure!(
                fs::metadata(&path)?.permissions().mode() & 0o111 != 0,
                "subject is not executable"
            );
            Ok(path)
        })
        .transpose()?;
    ensure!(!cases.is_empty(), "selection contains no cases");
    let vocab: BTreeSet<_> = cases
        .iter()
        .flat_map(|c| project.preparation_names(c))
        .collect();
    let vocab_review = vocab
        .iter()
        .map(|n| Ok((n.clone(), project.item(&format!("@vocab:{n}"))?.review)))
        .collect::<Result<_>>()?;
    let mut results = vec![];
    for case in cases {
        results.push(run_case(
            project,
            case,
            &platform,
            subject.as_deref(),
            options.keep,
            options.timeout,
        )?);
    }
    Ok(RunReport {
        engine_semantics: ENGINE_SEMANTICS,
        engine_review: project.item("@engine")?.review,
        vocab_review,
        platform,
        results,
    })
}
fn run_case(
    project: &Project,
    case: &Case,
    platform: &str,
    subject: Option<&Path>,
    keep: bool,
    default_timeout: Duration,
) -> Result<CaseResult> {
    let started = Instant::now();
    let review = project.item(&case.id)?.review;
    let root = match tempfile::Builder::new()
        .prefix(&format!("semspec-{}-", case.id))
        .tempdir()
    {
        Ok(root) => root,
        Err(error) => {
            return Ok(CaseResult {
                id: case.id.clone(),
                verdict: Verdict::Error {
                    message: error.to_string(),
                },
                review,
                duration_ms: started.elapsed().as_millis(),
                workdir: None,
            });
        }
    };
    let execute = || -> Result<(Option<i32>, String)> {
        let ws = root.path().join("ws");
        fs::create_dir(&ws)?;
        let mut script = String::from("set -euo pipefail\n");
        let vocab_dir = root.path().join("vocab");
        fs::create_dir(&vocab_dir)?;
        for name in project.preparation_names(case) {
            let path = vocab_dir.join(&name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, &project.preparation[&name].script)?;
            let quoted = shell_words::quote(path.to_str().context("vocabulary path must be UTF8")?);
            script.push_str(&format!("builtin source {quoted}\n"));
        }
        script.push_str(&case.script);
        let script_path = root.path().join("check.bash");
        fs::write(&script_path, script)?;
        let mut command = Command::new("bash");
        command
            .arg(&script_path)
            .current_dir(&ws)
            .env("CASE_ROOT", root.path())
            .env("WS", &ws)
            .env("SEMSPEC_BIN", std::env::current_exe()?)
            .env("SEMSPEC_PROJECT_ROOT", &project.root);
        command.env_remove("SUBJECT_BIN");
        if let Some(subject) = subject {
            command.env("SUBJECT_BIN", subject);
        }
        let log = root.path().join("case.log");
        let timeout = case
            .annotation
            .timeout
            .as_deref()
            .map(crate::parse::parse_timeout)
            .transpose()?
            .unwrap_or(default_timeout);
        let (status, timed_out) = execute_process(command, &log, timeout)?;
        let passed = status.success() && !timed_out;
        let detail = if passed {
            String::new()
        } else {
            format!(
                "{}{}",
                if timed_out { "timed out\n" } else { "" },
                tail(&log)?
            )
        };
        Ok((if timed_out { None } else { status.code() }, detail))
    };
    let expected =
        case.annotation.xfail_on.contains(platform) || case.annotation.xfail_on.contains("all");
    let verdict = match execute() {
        Ok((Some(77), reason)) => Verdict::Skip { reason },
        Ok((Some(0), _)) if expected => Verdict::XPass,
        Ok((Some(0), _)) => Verdict::Pass,
        Ok((_, output_tail)) if expected => Verdict::XFail {
            reason: case.annotation.xfail_reason.clone().unwrap(),
            output_tail,
        },
        Ok((_, output_tail)) => Verdict::Fail { output_tail },
        Err(error) => Verdict::Error {
            message: format!("{error:#}"),
        },
    };
    let workdir = if keep || verdict.retain() {
        Some(root.keep())
    } else {
        None
    };
    Ok(CaseResult {
        id: case.id.clone(),
        verdict,
        review,
        duration_ms: started.elapsed().as_millis(),
        workdir,
    })
}
#[cfg(unix)]
struct Group(i32);
#[cfg(unix)]
impl Group {
    fn signal(&self, signal: i32) {
        unsafe {
            libc::kill(-self.0, signal);
        }
    }
    fn alive(&self) -> bool {
        unsafe {
            libc::kill(-self.0, 0) == 0
                || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        }
    }
}
#[cfg(unix)]
impl Drop for Group {
    fn drop(&mut self) {
        self.signal(libc::SIGKILL);
    }
}
#[cfg(unix)]
pub fn execute_process(
    mut command: Command,
    log: &Path,
    timeout: Duration,
) -> Result<(ExitStatus, bool)> {
    let log = fs::File::create(log)?;
    command
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0);
    let mut child = command.spawn().context("launch interpreter")?;
    let group = Group(child.id() as i32);
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status, false));
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    group.signal(libc::SIGTERM);
    let grace = Instant::now() + Duration::from_secs(5);
    let mut status = None;
    while group.alive() && Instant::now() < grace {
        if status.is_none() {
            status = child.try_wait()?;
        }
        thread::sleep(Duration::from_millis(10));
    }
    group.signal(libc::SIGKILL);
    Ok((
        match status {
            Some(s) => s,
            None => child.wait()?,
        },
        true,
    ))
}
#[cfg(not(unix))]
pub fn execute_process(_: Command, _: &Path, _: Duration) -> Result<(ExitStatus, bool)> {
    anyhow::bail!("bash process groups require Unix")
}
fn tail(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(8192)))?;
    let mut bytes = vec![];
    file.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
