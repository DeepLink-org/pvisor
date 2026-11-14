//! Windows console policy stays in the frontend; the runtime only owns a
//! cancellable byte source and guest PTY dimensions.
use pvisor_containers::api::InputSource;
#[cfg(windows)]
use std::sync::atomic::Ordering;
use std::{io, sync::atomic::AtomicBool};

#[cfg(windows)]
use std::{
    os::windows::io::{AsRawHandle, BorrowedHandle},
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{ERROR_BROKEN_PIPE, ERROR_HANDLE_EOF, HANDLE, INVALID_HANDLE_VALUE},
    Storage::FileSystem::ReadFile,
    System::{Console::*, IO::CancelSynchronousIo},
};

pub(crate) struct Terminal {
    #[cfg(windows)]
    saved: Vec<(HANDLE, u32)>,
    #[cfg(windows)]
    pages: Option<(u32, u32)>,
}
impl Terminal {
    pub(crate) fn new(tty: bool, interactive: bool) -> anyhow::Result<Self> {
        #[cfg(windows)]
        {
            use std::io::IsTerminal;
            anyhow::ensure!(
                !tty || !interactive || std::io::stdin().is_terminal(),
                "the input device is not a TTY"
            );
            let mut terminal = Self {
                saved: Vec::new(),
                pages: None,
            };
            if tty {
                let input_page = unsafe { GetConsoleCP() };
                let output_page = unsafe { GetConsoleOutputCP() };
                if input_page != 0 && output_page != 0 {
                    terminal.pages = Some((input_page, output_page));
                    anyhow::ensure!(
                        unsafe { SetConsoleCP(65001) } != 0
                            && unsafe { SetConsoleOutputCP(65001) } != 0,
                        "enable UTF-8 console"
                    );
                }
                for kind in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                    let handle = unsafe { GetStdHandle(kind) };
                    let mut mode = 0;
                    if unsafe { GetConsoleMode(handle, &mut mode) } != 0 {
                        anyhow::ensure!(
                            unsafe {
                                SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING)
                            } != 0,
                            "enable terminal output: {}",
                            io::Error::last_os_error()
                        );
                        terminal.saved.push((handle, mode));
                    }
                }
                if interactive {
                    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
                    let mut mode = 0;
                    anyhow::ensure!(
                        unsafe { GetConsoleMode(handle, &mut mode) } != 0,
                        "read console mode"
                    );
                    let raw = (mode
                        & !(ENABLE_LINE_INPUT
                            | ENABLE_ECHO_INPUT
                            | ENABLE_PROCESSED_INPUT
                            | ENABLE_QUICK_EDIT_MODE))
                        | ENABLE_VIRTUAL_TERMINAL_INPUT
                        | ENABLE_EXTENDED_FLAGS;
                    anyhow::ensure!(
                        unsafe { SetConsoleMode(handle, raw) } != 0,
                        "enable terminal input: {}",
                        io::Error::last_os_error()
                    );
                    terminal.saved.push((handle, mode));
                }
            }
            Ok(terminal)
        }
        #[cfg(not(windows))]
        {
            let _ = (tty, interactive);
            anyhow::bail!("Windows console execution requires Windows")
        }
    }
    pub(crate) fn size() -> [u16; 2] {
        #[cfg(windows)]
        unsafe {
            let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
            if GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) != 0 {
                return [
                    (info.srWindow.Bottom - info.srWindow.Top + 1).max(1) as u16,
                    (info.srWindow.Right - info.srWindow.Left + 1).max(1) as u16,
                ];
            }
        }
        [24, 80]
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        #[cfg(windows)]
        for (handle, mode) in self.saved.iter().rev() {
            unsafe {
                SetConsoleMode(*handle, *mode);
            }
        }
        #[cfg(windows)]
        if let Some((input, output)) = self.pages {
            unsafe {
                SetConsoleCP(input);
                SetConsoleOutputCP(output);
            }
        }
    }
}

pub(crate) struct StdinSource {
    #[cfg(windows)]
    receiver: mpsc::Receiver<io::Result<Vec<u8>>>,
    #[cfg(windows)]
    pending: Vec<u8>,
    #[cfg(windows)]
    offset: usize,
    #[cfg(windows)]
    stop: Arc<AtomicBool>,
    #[cfg(windows)]
    worker: Option<thread::JoinHandle<()>>,
}
impl StdinSource {
    pub(crate) fn new() -> anyhow::Result<Self> {
        #[cfg(windows)]
        {
            let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            anyhow::ensure!(
                !handle.is_null() && handle != INVALID_HANDLE_VALUE,
                "stdin is unavailable"
            );
            let file = std::fs::File::from(
                unsafe { BorrowedHandle::borrow_raw(handle.cast()) }.try_clone_to_owned()?,
            );
            Ok(Self::from_file(file))
        }
        #[cfg(not(windows))]
        {
            anyhow::bail!("Windows stdin requires Windows")
        }
    }
    #[cfg(windows)]
    fn from_file(file: std::fs::File) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let worker = thread::spawn(move || {
            while !flag.load(Ordering::Acquire) {
                let mut bytes = vec![0; 16384];
                let mut size = 0;
                let ok = unsafe {
                    ReadFile(
                        file.as_raw_handle().cast(),
                        bytes.as_mut_ptr(),
                        bytes.len() as u32,
                        &mut size,
                        std::ptr::null_mut(),
                    )
                };
                let eof = size == 0;
                let mut result = if ok != 0 {
                    bytes.truncate(size as usize);
                    Ok(bytes)
                } else {
                    let error = io::Error::last_os_error();
                    if matches!(error.raw_os_error(), Some(code) if code == ERROR_BROKEN_PIPE as i32 || code == ERROR_HANDLE_EOF as i32)
                    {
                        Ok(Vec::new())
                    } else {
                        Err(error)
                    }
                };
                loop {
                    if flag.load(Ordering::Acquire) {
                        return;
                    }
                    match sender.try_send(result) {
                        Ok(()) => break,
                        Err(mpsc::TrySendError::Full(value)) => {
                            result = value;
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(mpsc::TrySendError::Disconnected(_)) => return,
                    }
                }
                if eof || ok == 0 {
                    break;
                }
            }
        });
        Self {
            receiver,
            pending: Vec::new(),
            offset: 0,
            stop,
            worker: Some(worker),
        }
    }
}
impl InputSource for StdinSource {
    fn read(&mut self, buffer: &mut [u8], cancel: &AtomicBool) -> io::Result<usize> {
        #[cfg(windows)]
        loop {
            if cancel.load(Ordering::Acquire) {
                return Ok(0);
            }
            if self.offset < self.pending.len() {
                let size = buffer.len().min(self.pending.len() - self.offset);
                buffer[..size].copy_from_slice(&self.pending[self.offset..self.offset + size]);
                self.offset += size;
                return Ok(size);
            }
            match self.receiver.recv_timeout(Duration::from_millis(20)) {
                Ok(Ok(bytes)) if bytes.is_empty() => return Ok(0),
                Ok(Ok(bytes)) => {
                    self.pending = bytes;
                    self.offset = 0;
                }
                Ok(Err(error)) => return Err(error),
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (buffer, cancel);
            Err(io::ErrorKind::Unsupported.into())
        }
    }
}
impl Drop for StdinSource {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(worker) = self.worker.take() {
            self.stop.store(true, Ordering::Release);
            // Cancellation can race the transition into ReadFile. Repeat until
            // the worker observes the latched stop flag, never requiring another
            // user keystroke to release a completed guest's stdin.
            while !worker.is_finished() {
                unsafe {
                    CancelSynchronousIo(worker.as_raw_handle().cast());
                }
                thread::sleep(Duration::from_millis(5));
            }
            let _ = worker.join();
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{io::Write, os::windows::io::FromRawHandle, sync::mpsc, time::Instant};
    use windows_sys::Win32::System::Pipes::CreatePipe;

    fn pipe() -> (std::fs::File, std::fs::File) {
        let (mut read, mut write) = (std::ptr::null_mut(), std::ptr::null_mut());
        assert_ne!(
            unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) },
            0
        );
        unsafe {
            (
                std::fs::File::from_raw_handle(read.cast()),
                std::fs::File::from_raw_handle(write.cast()),
            )
        }
    }

    #[test]
    fn piped_input_preserves_bytes_and_reports_eof() {
        let (read, mut write) = pipe();
        let mut source = StdinSource::from_file(read);
        write.write_all(b"literal \0 \xff").unwrap();
        drop(write);
        let cancel = AtomicBool::new(false);
        let mut result = Vec::new();
        let mut buffer = [0; 3];
        loop {
            let count = source.read(&mut buffer, &cancel).unwrap();
            if count == 0 {
                break;
            }
            result.extend_from_slice(&buffer[..count]);
        }
        assert_eq!(result, b"literal \0 \xff");
    }

    #[test]
    fn cancellation_and_drop_do_not_require_idle_input_eof() {
        let (read, _open_writer) = pipe();
        let mut source = StdinSource::from_file(read);
        let started = Instant::now();
        assert_eq!(source.read(&mut [0; 1], &AtomicBool::new(true)).unwrap(), 0);
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            drop(source);
            done.send(()).unwrap();
        });
        finished.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
