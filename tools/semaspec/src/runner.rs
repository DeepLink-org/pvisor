use crate::{
    model::{Case, CaseResult, Evidence, RunReport, Verdict},
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
    sync::atomic::{AtomicI32, Ordering},
    thread,
    time::{Duration, Instant},
};

static INTERRUPTED: AtomicI32 = AtomicI32::new(0);
extern "C" fn interrupted(signal: libc::c_int) {
    INTERRUPTED.store(signal, Ordering::Relaxed);
}
/// Installed by the CLI, not library users. The handler only sets a lock-free flag.
pub fn install_signal_handlers() -> Result<()> {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = interrupted as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGINT, libc::SIGTERM] {
            ensure!(
                libc::sigaction(signal, &action, std::ptr::null_mut()) == 0,
                "install cancellation handler"
            );
        }
    }
    Ok(())
}
pub fn interruption() -> i32 {
    INTERRUPTED.load(Ordering::Relaxed)
}

pub struct Options {
    pub timeout: Duration,
    pub subject: Option<PathBuf>,
    pub keep: bool,
    /// Optional existing directory in which retained case workspaces are created.
    pub artifact_root: Option<PathBuf>,
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
        if interruption() != 0 {
            break;
        }
        results.push(run_case(
            project,
            case,
            &platform,
            subject.as_deref(),
            options,
        )?);
    }
    Ok(RunReport {
        benchmarks: vec![],
        engine_semantics: ENGINE_SEMANTICS.into(),
        engine_review: project.item("@engine")?.review,
        vocab_review,
        platform,
        results,
        root: Some(project.root.clone()),
    })
}
fn run_case(
    project: &Project,
    case: &Case,
    platform: &str,
    subject: Option<&Path>,
    options: &Options,
) -> Result<CaseResult> {
    let started = Instant::now();
    let review = project.item(&case.id)?.review;
    let mut builder = tempfile::Builder::new();
    let prefix = format!("semaspec-{}-", case.id);
    builder.prefix(&prefix);
    let directory = match &options.artifact_root {
        Some(path) => builder.tempdir_in(path),
        None => builder.tempdir(),
    };
    let root = match directory {
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
                evidence: None,
            });
        }
    };
    let mut evidence = Some(Evidence {
        digest: project.case_digest(case),
        stdout: String::new(),
        stderr: String::new(),
        exit_code: None,
        timed_out: false,
        cancelled: None,
        truncated: false,
    });
    let mut execute = || -> Result<(Option<i32>, String)> {
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
        let out = root.path().join("stdout.log");
        let err = root.path().join("stderr.log");
        let timeout = case
            .annotation
            .timeout
            .as_deref()
            .map(crate::parse::parse_timeout)
            .transpose()?
            .unwrap_or(options.timeout);
        let (status, timed_out) = execute_streams(command, &out, &err, timeout)?;
        let read = |path: &Path| -> Result<(String, bool)> {
            let mut bytes = Vec::new();
            fs::File::open(path)?.take(65536).read_to_end(&mut bytes)?;
            Ok((
                String::from_utf8_lossy(&bytes).into_owned(),
                fs::metadata(path)?.len() > bytes.len() as u64,
            ))
        };
        let (stdout, out_truncated) = read(&out)?;
        let (stderr, err_truncated) = read(&err)?;
        fs::write(&log, format!("{stdout}{stderr}"))?;
        evidence = Some(Evidence {
            digest: project.case_digest(case),
            stdout,
            stderr,
            exit_code: status.code(),
            timed_out: timed_out && interruption() == 0,
            cancelled: (interruption() != 0).then(interruption),
            truncated: out_truncated || err_truncated,
        });
        let aborted = timed_out || interruption() != 0;
        if status.success() && !aborted {
            for expected in &case.expectations {
                let path = if expected.stream == "stdout" {
                    &out
                } else {
                    &err
                };
                let matches = fs::metadata(path)?.len() == expected.text.len() as u64
                    && fs::read(path)? == expected.text.as_bytes();
                if !matches {
                    return Ok((
                        Some(1),
                        format!("{} differs from the exact expected bytes", expected.stream),
                    ));
                }
            }
        }
        let passed = status.success() && !aborted;
        let detail = if passed {
            String::new()
        } else {
            format!(
                "{}stdout tail:\n{}\nstderr tail:\n{}",
                if interruption() != 0 {
                    "cancelled by signal\n"
                } else if timed_out {
                    "timed out\n"
                } else {
                    ""
                },
                tail(&out)?,
                tail(&err)?
            )
        };
        Ok((if aborted { None } else { status.code() }, detail))
    };
    let expected =
        case.annotation.xfail_on.contains(platform) || case.annotation.xfail_on.contains("all");
    let verdict = match execute() {
        Ok((Some(77), reason)) => Verdict::Skip { reason },
        Ok((Some(0), _)) if expected => Verdict::XPass,
        Ok((Some(0), _)) => Verdict::Pass,
        // Infrastructure exits and timeouts must not satisfy a known product violation.
        Ok((Some(code), output_tail)) if expected && ![126, 127].contains(&code) => {
            Verdict::XFail {
                reason: case.annotation.xfail_reason.clone().unwrap(),
                output_tail,
            }
        }
        Ok((_, output_tail)) => Verdict::Fail { output_tail },
        Err(error) => Verdict::Error {
            message: format!("{error:#}"),
        },
    };
    let workdir = if options.keep || verdict.retain() {
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
        evidence,
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
    command: Command,
    log: &Path,
    timeout: Duration,
) -> Result<(ExitStatus, bool)> {
    let log = fs::File::create(log)?;
    execute_files(command, log.try_clone()?, log, timeout)
}
pub fn execute_streams(
    command: Command,
    stdout: &Path,
    stderr: &Path,
    timeout: Duration,
) -> Result<(ExitStatus, bool)> {
    execute_files(
        command,
        fs::File::create(stdout)?,
        fs::File::create(stderr)?,
        timeout,
    )
}
fn execute_files(
    mut command: Command,
    stdout: fs::File,
    stderr: fs::File,
    timeout: Duration,
) -> Result<(ExitStatus, bool)> {
    command
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .process_group(0);
    let mut child = command.spawn().context("launch interpreter")?;
    let group = Group(child.id() as i32);
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status, false));
        }
        if Instant::now() >= deadline || interruption() != 0 {
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
