//! Apply presentation and terminal ownership. Runtime owns planning and mutation.
use super::runtime::ApplyArgs;
use anyhow::Context;
use pvisor::job_service::{
    ApplyCommandRequest, ApplyCommandResponse, JobSelection, RuntimeJobService,
};
use pvisor::{ApplySelection, RunRecord};
use pvisor_core::overlay::{ChangeEntry, ChangeEntryType};
use std::io::{IsTerminal, Write};
use std::path::{Component, Path, PathBuf};

pub(super) fn run(mut args: ApplyArgs) -> i32 {
    let positive = args.all || !args.paths.is_empty() || !args.include.is_empty();
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let picker = args.interactive || (!positive && args.exclude.is_empty() && tty);
    let invalid_selection = args
        .paths
        .iter()
        .map(PathBuf::as_path)
        .chain(args.include.iter().chain(&args.exclude).map(Path::new))
        .any(|path| {
            path.as_os_str().is_empty()
                || path.is_absolute()
                || path.components().any(|c| {
                    matches!(
                        c,
                        Component::ParentDir | Component::RootDir | Component::Prefix(_)
                    )
                })
        });
    let invalid = if invalid_selection {
        Some("apply selection must be relative without '..'")
    } else if args.json && !positive {
        Some("--json requires --all, --path or --include")
    } else if picker && !tty {
        Some("interactive apply requires both stdin and stdout to be terminals")
    } else if picker && (args.dry_run || args.json) {
        Some("interactive apply cannot use --dry-run or --json")
    } else if !positive && !picker {
        Some("apply requires --all, --path or --include (or --interactive on a terminal)")
    } else {
        None
    };
    if let Some(message) = invalid {
        render_error(args.json, args.dry_run, message);
        return 2;
    }
    let result = (|| {
        args.selector =
            super::stage_source::resolve(Some(&args.selector), &args.store, args.mount.as_deref())?
                .expect("explicit apply source");
        args.mount = None;
        if picker {
            interactive(&mut args)
        } else {
            request(&args, args.dry_run, None).and_then(|response| {
                render(&response, args.json);
                if args.diff && exit_code(&response) == 0 {
                    print_diff(&response)?;
                }
                Ok(exit_code(&response))
            })
        }
    })();
    match result {
        Ok(code) => code,
        Err(error) => {
            render_error(args.json, args.dry_run, &format!("{error:#}"));
            1
        }
    }
}

pub(super) fn render_error(json: bool, dry_run: bool, message: &str) {
    eprintln!("{message}");
    if json {
        println!(
            "{}",
            serde_json::json!({
                "schema_version": 1, "operation": "apply",
                "mode": if dry_run { "preview" } else { "execute" }, "status": "error",
                "source": null, "scope": null, "selected": [], "required": [],
                "blockers": [message], "remaining": {"in_scope": null, "outside_scope": null},
                "execution": null, "candidates": [], "plan": null
            })
        );
    }
}

fn request(
    args: &ApplyArgs,
    dry_run: bool,
    expected_plan: Option<String>,
) -> anyhow::Result<ApplyCommandResponse> {
    RuntimeJobService::apply_command(
        &super::host::service_context(),
        ApplyCommandRequest {
            job: JobSelection {
                selector: Some(args.selector.clone()),
                storage: args.store.clone(),
            },

            rootfs: args.rootfs,
            selection: ApplySelection {
                paths: args.paths.clone(),
                includes: args.include.clone(),
                excludes: args.exclude.clone(),
            },
            all: args.all,
            dry_run,
            expected_plan,
        },
    )
}

fn exit_code(response: &ApplyCommandResponse) -> i32 {
    match response.status.as_str() {
        "ready" | "preview" | "applied" | "no_changes" => 0,
        "blocked" => 3,
        _ => 1,
    }
}

fn render(response: &ApplyCommandResponse, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string(response).expect("apply response serialization")
        );
        for blocker in &response.blockers {
            eprintln!("{blocker}");
        }
        return;
    }
    println!("apply: {}", response.status);
    println!("Source: {}", response.source.job_id);
    println!("Stage: {}", response.source.stage.display());
    if let Some(target) = &response.scope.target {
        println!("{} target: {}", response.scope.kind, target.display());
    }
    for (name, changes) in [
        ("selected", &response.selected),
        ("required", &response.required),
    ] {
        println!("{name}: {}", changes.len());
        for change in changes {
            println!("  {:?} {:?}", change.kind, change.path);
        }
    }
    println!(
        "remaining: in_scope={:?}, outside_scope={:?}",
        response.remaining.in_scope, response.remaining.outside_scope
    );
    for blocker in &response.blockers {
        eprintln!("blocker: {blocker}");
    }
    if let Some(execution) = &response.execution {
        if let Some(apply_id) = &execution.apply_id {
            println!("Apply ID: {apply_id}");
        }
        println!(
            "execution: applied={:?}, ledger_confirmed={}, recovery_required={}",
            execution.applied, execution.ledger_confirmed, execution.recovery_required
        );
        if let Some(error) = &execution.error {
            eprintln!("{error}");
        }
    }
    if response.execution.is_some()
        && (response.status == "error"
            || response
                .execution
                .as_ref()
                .is_some_and(|execution| execution.error.is_some() || execution.recovery_required))
    {
        println!(
            "Some target changes may have been written. Stage and recovery evidence are retained; inspect and reconcile before retrying."
        );
    } else if response.status == "blocked" && response.execution.is_none() {
        println!("No target changes were made (blocked during preflight).");
    } else if response.status == "no_changes"
        || (matches!(response.status.as_str(), "ready" | "preview")
            && response.selected.is_empty()
            && response.required.is_empty())
    {
        println!("No matching staged changes");
    } else if response.status == "applied" {
        println!(
            "Apply succeeded. Tests were not run automatically. Apply is not a transaction across files; validate the result separately."
        );
    }
}

struct Terminal(libc::termios);
impl Terminal {
    fn acquire() -> anyhow::Result<Self> {
        let mut saved = std::mem::MaybeUninit::uninit();
        anyhow::ensure!(
            unsafe { libc::tcgetattr(0, saved.as_mut_ptr()) } == 0,
            "read terminal mode: {}",
            std::io::Error::last_os_error()
        );
        let saved = unsafe { saved.assume_init() };
        let mut mode = saved;
        // Keep output translation, but receive Ctrl-C as a key so Drop restores mode.
        mode.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
        mode.c_iflag &= !(libc::IXON | libc::ICRNL);
        mode.c_cc[libc::VMIN] = 1;
        mode.c_cc[libc::VTIME] = 0;
        anyhow::ensure!(
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &mode) } == 0,
            "set terminal mode: {}",
            std::io::Error::last_os_error()
        );
        Ok(Self(saved))
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.0);
        }
    }
}
fn key() -> anyhow::Result<u8> {
    std::io::stdout().flush()?;
    let mut byte = [0];
    loop {
        let count = unsafe { libc::read(0, byte.as_mut_ptr().cast(), 1) };
        if count == 1 {
            return Ok(byte[0]);
        }
        if count == 0 {
            anyhow::bail!("terminal input closed");
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
}

fn sequence_key() -> anyhow::Result<Option<u8>> {
    let mut input = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut input, 1, 100) };
    if ready < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if ready == 0 || input.revents & libc::POLLIN == 0 {
        return Ok(None);
    }
    key().map(Some)
}

fn interactive(args: &mut ApplyArgs) -> anyhow::Result<i32> {
    'refresh: loop {
        let explicit = args.all || !args.paths.is_empty() || !args.include.is_empty();
        let initial = if explicit {
            Some(request(args, true, None)?)
        } else {
            None
        };
        if let Some(response) = &initial {
            args.selector = response.source.stage.clone();
        }
        let mut inventory_args = args.clone();
        inventory_args.all = true;
        inventory_args.paths.clear();
        inventory_args.include.clear();
        let inventory = request(&inventory_args, true, None)?;
        args.selector = inventory.source.stage.clone();
        if inventory.status == "blocked"
            && (inventory.plan.is_none()
                || inventory.candidates.is_empty()
                || inventory
                    .blockers
                    .iter()
                    .any(|b| b.contains("pending apply") || b.contains("stage was dropped")))
        {
            render(&inventory, false);
            return Ok(3);
        }
        for blocker in &inventory.blockers {
            println!("inventory blocker: {blocker}");
        }
        let candidates = inventory.candidates;
        let mut enabled = vec![true; candidates.len()];
        // Ask the same planner, not a second glob implementation. A no-match result
        // means excluded; other blockers remain visible at confirmation and cannot apply.
        if !args.exclude.is_empty() {
            for (index, change) in candidates.iter().enumerate() {
                let mut probe = args.clone();
                probe.all = false;
                probe.paths = vec![change.relative_path()];
                probe.include.clear();
                let response = request(&probe, true, None)?;
                let no_match = response
                    .blockers
                    .iter()
                    .any(|b| b.contains("no staged changes matched"))
                    || (matches!(response.status.as_str(), "ready" | "preview")
                        && response.selected.is_empty()
                        && response.required.is_empty());
                enabled[index] = !no_match;
            }
        }
        let mut selected: Vec<bool> = candidates
            .iter()
            .enumerate()
            .map(|(index, c)| {
                enabled[index]
                    && initial.as_ref().is_some_and(|p| {
                        p.selected
                            .iter()
                            .any(|s| s.relative_path() == c.relative_path())
                    })
            })
            .collect();
        let _terminal = Terminal::acquire()?;
        let mut cursor = 0usize;
        loop {
            println!(
                "\nChoose paths: ↑/↓ move, space toggle, a all, n none, d diff, Enter preview, q quit"
            );
            for (i, change) in candidates.iter().enumerate() {
                println!(
                    "{} [{}] {:?}{}",
                    if i == cursor { ">" } else { " " },
                    if !enabled[i] {
                        "-"
                    } else if selected[i] {
                        "x"
                    } else {
                        " "
                    },
                    change.path,
                    if enabled[i] { "" } else { " (excluded)" }
                );
            }
            let input = key()?;
            match input {
                b'q' | 3 | 4 => return Ok(130),
                27 => {
                    if sequence_key()? == Some(b'[') {
                        match sequence_key()? {
                            Some(b'A') => cursor = cursor.saturating_sub(1),
                            Some(b'B') if cursor + 1 < candidates.len() => cursor += 1,
                            _ => {}
                        }
                    }
                }
                b' ' if cursor < candidates.len() && enabled[cursor] => {
                    selected[cursor] = !selected[cursor]
                }
                b'a' => selected.clone_from(&enabled),
                b'n' => selected.fill(false),
                b'd' | b'\r' | b'\n' => {
                    let pressed = key_selection(args, &candidates, &selected);
                    if !pressed {
                        println!("Select at least one path.");
                        continue;
                    }
                    let mut preview = request(args, true, None)?;
                    render(&preview, false);
                    if input == b'd' {
                        print_diff(&preview)?;
                    }
                    loop {
                        println!(
                            "[a] apply  [b] back  [d] diff  [q] quit{}",
                            if exit_code(&preview) == 3 {
                                " (apply blocked)"
                            } else {
                                ""
                            }
                        );
                        match key()? {
                            b'q' | 3 | 4 => return Ok(130),
                            b'b' => continue 'refresh,
                            b'd' => print_diff(&preview)?,
                            b'a' if exit_code(&preview) == 0 && preview.plan.is_some() => {
                                let execution = request(args, false, preview.plan.clone())?;
                                render(&execution, false);
                                if execution.status == "blocked" {
                                    // Never execute a changed plan without showing a fresh preview
                                    // and requiring another explicit keypress.
                                    preview = request(args, true, None)?;
                                    render(&preview, false);
                                } else {
                                    return Ok(exit_code(&execution));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
fn key_selection(args: &mut ApplyArgs, candidates: &[ChangeEntry], selected: &[bool]) -> bool {
    args.all = false;
    args.include.clear();
    args.paths = candidates
        .iter()
        .zip(selected)
        .filter(|(_, yes)| **yes)
        .map(|(c, _)| c.relative_path())
        .collect();
    !args.paths.is_empty()
}

fn print_diff(response: &ApplyCommandResponse) -> anyhow::Result<()> {
    let record = RunRecord::read(&response.source.stage)?;
    let overlay = record
        .overlay
        .as_ref()
        .context("missing overlay for diff")?;
    let target = response
        .scope
        .target
        .as_ref()
        .context("missing target for diff")?;
    let prefix = if overlay.target == Path::new("/") && response.scope.kind == "project" {
        target.strip_prefix("/")?.to_path_buf()
    } else {
        PathBuf::new()
    };
    let mut remaining = 256 * 1024;
    for change in &response.selected {
        if remaining == 0 {
            println!("diff truncated at 262144 bytes");
            break;
        }
        let relative = change.relative_path();
        anyhow::ensure!(
            relative
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
            "unsafe diff path"
        );
        let old = if change.old_type == Some(ChangeEntryType::File) {
            super::diff::read_regular(target, &relative, 1024 * 1024)
        } else {
            Ok(vec![])
        };
        let new = if change.new_type == Some(ChangeEntryType::File) {
            super::diff::read_regular(overlay.upper.path(), &prefix.join(&relative), 1024 * 1024)
        } else {
            Ok(vec![])
        };
        let (Ok(old), Ok(new)) = (old, new) else {
            println!(
                "{:?}: content diff skipped (unsafe, unreadable, special or large file)",
                change.path
            );
            continue;
        };
        if !super::diff::is_text(&old) || !super::diff::is_text(&new) {
            println!("{:?}: binary (text diff unavailable)", change.path);
            continue;
        }
        if old.is_empty() && new.is_empty() {
            println!(
                "{:?}: metadata/symlink/directory change (no text diff)",
                change.path
            );
            continue;
        }
        let output = super::diff::unified(&old, &new, &change.path)?;
        let keep = remaining.min(output.len());
        print!("{}", String::from_utf8_lossy(&output[..keep]));
        remaining -= keep;
    }
    Ok(())
}
