//! Independent owner creation never inherits the frontend's redirected pipes.
use super::ipc;
use anyhow::{Context, ensure};
use std::{path::Path, ptr};
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CreateProcessW, DETACHED_PROCESS,
    PROCESS_INFORMATION, STARTUPINFOW,
};

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

pub(super) fn owner(root: &Path, id: &str, generation: &str) -> anyhow::Result<()> {
    let executable = std::env::current_exe()?;
    let executable = executable
        .to_str()
        .context("executable path must be UTF-8")?;
    let root = root.to_str().context("store path must be UTF-8")?;
    let mut line = ipc::wide(&format!(
        "{} --pvisor-container-owner {} {} {}",
        quote(executable),
        quote(root),
        quote(id),
        quote(generation)
    ));
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of_val(&startup) as u32;
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // bInheritHandles=FALSE is intentional. Detached output must reach EOF as
    // soon as the frontend exits, even when its caller uses redirected pipes.
    ensure!(
        unsafe {
            CreateProcessW(
                ipc::wide(executable).as_ptr(),
                line.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB,
                ptr::null(),
                ptr::null(),
                &startup,
                &mut process,
            )
        } != 0,
        "start independent container owner: {}",
        std::io::Error::last_os_error()
    );
    let _process = ipc::Handle(process.hProcess);
    let _thread = ipc::Handle(process.hThread);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quotes_windows_arguments_without_shell_interpretation() {
        assert_eq!(quote("plain"), "\"plain\"");
        assert_eq!(quote("a b"), "\"a b\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote("C:\\a b\\"), "\"C:\\a b\\\\\"");
    }
}
