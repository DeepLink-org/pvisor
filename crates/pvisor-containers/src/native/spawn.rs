//! Independent owner creation never inherits the frontend's redirected pipes.
use super::ipc;
use anyhow::Context;
use std::{io, path::Path, ptr};
use windows_sys::Win32::{
    Foundation::ERROR_ACCESS_DENIED,
    System::{
        JobObjects::IsProcessInJob,
        Threading::{
            CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CreateProcessW, DETACHED_PROCESS,
            GetCurrentProcess, PROCESS_INFORMATION, STARTUPINFOW,
        },
    },
};

struct Spawned {
    _process: ipc::Handle,
    _thread: ipc::Handle,
}

fn quote(argument: &str) -> String {
    let mut output = String::from("\"");
    let mut slashes = 0;
    for c in argument.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        if c == '"' {
            output.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
        } else {
            output.extend(std::iter::repeat_n('\\', slashes));
        }
        slashes = 0;
        output.push(c);
    }
    output.extend(std::iter::repeat_n('\\', slashes * 2));
    output.push('"');
    output
}

fn create(executable: &str, line: &[u16], flags: u32) -> io::Result<Spawned> {
    // CreateProcessW may modify the command line, including on failure.
    // A contained retry must receive the original arguments.
    let mut line = line.to_vec();
    let executable = ipc::wide(executable);
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of_val(&startup) as u32;
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // bInheritHandles=FALSE is intentional. Detached output must reach EOF as
    // soon as the frontend exits, even when its caller uses redirected pipes.
    if unsafe {
        CreateProcessW(
            executable.as_ptr(),
            line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            flags,
            ptr::null(),
            ptr::null(),
            &startup,
            &mut process,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(Spawned {
        _process: ipc::Handle(process.hProcess),
        _thread: ipc::Handle(process.hThread),
    })
}

fn detached(executable: &str, line: &[u16]) -> io::Result<Spawned> {
    let flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    match create(executable, line, flags | CREATE_BREAKAWAY_FROM_JOB) {
        Ok(process) => Ok(process),
        Err(error) => {
            let mut in_job = 0;
            // Hosted runners and some shells prohibit breakaway. Preserve
            // their containment rather than requiring permission to escape.
            // Unknown membership or any other error retains the original
            // failure; no weaker handle inheritance or token is attempted.
            if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32)
                && unsafe { IsProcessInJob(GetCurrentProcess(), ptr::null_mut(), &mut in_job) } != 0
                && in_job != 0
            {
                create(executable, line, flags)
            } else {
                Err(error)
            }
        }
    }
}

pub(super) fn owner(root: &Path, id: &str, generation: &str) -> anyhow::Result<()> {
    let executable = std::env::current_exe()?;
    let executable = executable
        .to_str()
        .context("executable path must be UTF-8")?;
    let root = root.to_str().context("store path must be UTF-8")?;
    let line = ipc::wide(&format!(
        "{} --pvisor-container-owner {} {} {}",
        quote(executable),
        quote(root),
        quote(id),
        quote(generation)
    ));
    let _owner = detached(executable, &line).context("start independent container owner")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::windows::process::CommandExt,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    use windows_sys::Win32::System::{
        JobObjects::{AssignProcessToJobObject, CreateJobObjectW},
        Threading::{CREATE_SUSPENDED, GetExitCodeProcess, WaitForSingleObject},
    };

    #[test]
    fn restricted_outer_job_allows_contained_owner_and_nested_runner_cleanup() {
        // No KILL_ON_CLOSE on the fixture job: the test process itself joins
        // it. Nextest isolates this irreversible per-process membership.
        let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        assert!(!raw.is_null());
        let outer = ipc::Handle(raw);
        assert_ne!(
            unsafe { AssignProcessToJobObject(outer.0, GetCurrentProcess()) },
            0
        );
        let executable = std::env::var("SystemRoot").unwrap() + r"\System32\cmd.exe";
        let line = ipc::wide(&format!("{} /d /c exit 7", quote(&executable)));
        let flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        let error = create(&executable, &line, flags | CREATE_BREAKAWAY_FROM_JOB)
            .err()
            .expect("fixture must prohibit breakaway");
        assert_eq!(error.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
        let child = detached(&executable, &line).unwrap();
        assert_eq!(unsafe { WaitForSingleObject(child._process.0, 5000) }, 0);
        let mut code = 0;
        assert_ne!(
            unsafe { GetExitCodeProcess(child._process.0, &mut code) },
            0
        );
        assert_eq!(code, 7);

        let mut runner = Command::new(executable)
            .args(["/d", "/c", "ping -n 60 127.0.0.1 >NUL"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .unwrap();
        let job = super::super::job::own_and_resume(&mut runner).unwrap();
        assert!(runner.try_wait().unwrap().is_none());
        drop(job);
        let deadline = Instant::now() + Duration::from_secs(5);
        while runner.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = runner.kill();
                let _ = runner.wait();
                panic!("nested runner survived its owner's Job");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn quotes_windows_arguments_without_shell_interpretation() {
        assert_eq!(quote("plain"), "\"plain\"");
        assert_eq!(quote("a b"), "\"a b\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote("C:\\a b\\"), "\"C:\\a b\\\\\"");
    }
}
