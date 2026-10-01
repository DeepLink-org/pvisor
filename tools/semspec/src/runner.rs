use crate::{
    config::Probe,
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
    pub subject: Option<PathBuf>,
    pub keep: bool,
    pub case_ids: Vec<String>,
    pub domain: Option<String>,
}
pub fn run(project: &Project, options: &Options) -> Result<RunReport> {
    let platform = project.config.platform()?;
    let subject = match &options.subject {
        Some(path) => path.clone(),
        None => project.root.join(&project.config.subject.bin),
    }
    .canonicalize()
    .context("subject binary unavailable")?;
    ensure!(subject.is_file(), "subject must be a file");
    #[cfg(unix)]
    ensure!(
        fs::metadata(&subject)?.permissions().mode() & 0o111 != 0,
        "subject is not executable"
    );
    for id in &options.case_ids {
        ensure!(
            project.cases.iter().any(|c| c.id == *id),
            "unknown case {id}"
        );
    }
    if let Some(domain) = &options.domain {
        ensure!(
            project.cases.iter().any(|c| c.domain == *domain),
            "unknown domain {domain}"
        );
    }
    let cases: Vec<_> = project
        .cases
        .iter()
        .filter(|c| {
            (options.case_ids.is_empty() || options.case_ids.contains(&c.id))
                && options.domain.as_ref().is_none_or(|d| c.domain == *d)
        })
        .collect();
    ensure!(!cases.is_empty(), "selection contains no cases");
    let vocab: BTreeSet<_> = cases.iter().flat_map(|c| project.vocab_names(c)).collect();
    let vocab_review = vocab
        .iter()
        .map(|n| Ok((n.clone(), project.item(&format!("@vocab:{n}"))?.review)))
        .collect::<Result<_>>()?;
    let mut results = vec![];
    for case in cases {
        results.push(run_case(project, case, &platform, &subject, options.keep)?);
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
    subject: &Path,
    keep: bool,
) -> Result<CaseResult> {
    let started = Instant::now();
    let review = project.item(&case.id)?.review;
    for requirement in &case.annotation.requires {
        let probe = project.config.requirements[requirement].get(platform);
        let result = probe
            .map(|p| check_probe(p, project))
            .unwrap_or_else(|| Err(format!("not defined on {platform}")));
        if let Err(reason) = result {
            return Ok(CaseResult {
                id: case.id.clone(),
                verdict: Verdict::Skip {
                    requirement: requirement.clone(),
                    reason,
                },
                review,
                duration_ms: started.elapsed().as_millis(),
                workdir: None,
            });
        }
    }
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
    let execute = || -> Result<(bool, String)> {
        let ws = root.path().join("ws");
        fs::create_dir(&ws)?;
        let mut script = String::from("set -euo pipefail\n");
        for name in project.vocab_names(case) {
            script.push_str(&project.vocab[&name].text);
            script.push('\n');
        }
        script.push_str(&case.script);
        let script_path = root.path().join("check.bash");
        fs::write(&script_path, script)?;
        let mut command = Command::new("bash");
        command
            .arg(&script_path)
            .current_dir(&ws)
            .envs(&project.config.subject.env)
            .env("SUBJECT_BIN", subject)
            .env("CASE_ROOT", root.path())
            .env("WS", &ws)
            .env("SEMSPEC_BIN", std::env::current_exe()?)
            .env("SEMSPEC_PROJECT_ROOT", &project.root);
        let log = root.path().join("case.log");
        let (status, timed_out) = execute_process(command, &log, project.config.timeout()?)?;
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
        Ok((passed, detail))
    };
    let expected =
        case.annotation.xfail_on.contains(platform) || case.annotation.xfail_on.contains("all");
    let verdict = match execute() {
        Ok((true, _)) if expected => Verdict::XPass,
        Ok((true, _)) => Verdict::Pass,
        Ok((false, output_tail)) if expected => Verdict::XFail {
            reason: case.annotation.xfail_reason.clone().unwrap(),
            output_tail,
        },
        Ok((false, output_tail)) => Verdict::Fail { output_tail },
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
fn check_probe(probe: &Probe, project: &Project) -> Result<(), String> {
    if let Some(path) = &probe.path_exists {
        return if project.root.join(path).exists() {
            Ok(())
        } else {
            Err(format!("{} does not exist", path.display()))
        };
    }
    if let Some(argv) = &probe.command_succeeds {
        let check = || -> Result<bool> {
            let root = tempfile::tempdir()?;
            let mut command = Command::new(&argv[0]);
            command
                .args(&argv[1..])
                .current_dir(&project.root)
                .envs(&project.config.subject.env);
            let (status, timed_out) = execute_process(
                command,
                &root.path().join("probe.log"),
                project.config.timeout()?.min(Duration::from_secs(15)),
            )?;
            Ok(status.success() && !timed_out)
        };
        return match check() {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!("probe failed: {argv:?}")),
            Err(e) => Err(format!("probe unavailable: {e:#}")),
        };
    }
    if let Some(all) = &probe.all {
        for p in all {
            check_probe(p, project)?;
        }
        return Ok(());
    }
    if let Some(any) = &probe.any {
        let mut errors = vec![];
        for p in any {
            match check_probe(p, project) {
                Ok(()) => return Ok(()),
                Err(e) => errors.push(e),
            }
        }
        return Err(errors.join("; "));
    }
    Err("invalid probe".into())
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
    let mut child = command.spawn().context("launch interpreter/probe")?;
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
