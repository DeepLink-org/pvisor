//! Same-user, local-only Windows byte pipes. Names are launch identities, not
//! bearer secrets. Each accepted connection owns its handle until reply drain.
use anyhow::{Context, ensure};
use std::{
    ffi::c_void,
    fs::File,
    io::{self, Read, Write},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    },
    path::Path,
    ptr,
    sync::Arc,
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
        LocalFree,
    },
    Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    },
    Security::{GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser},
    Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FlushFileBuffers, PIPE_ACCESS_DUPLEX,
        ReadFile, WriteFile,
    },
    System::IO::CancelIoEx,
    System::IO::{GetOverlappedResult, OVERLAPPED},
    System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    },
    System::Threading::{
        CreateEventW, GetCurrentProcess, OpenProcessToken, SetEvent, WaitForMultipleObjects,
    },
};

pub(super) fn wide(value: &str) -> Vec<u16> {
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(Some(0))
        .collect()
}
pub(super) struct Handle(pub HANDLE);
unsafe impl Send for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct Security(*mut c_void);
impl Drop for Security {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
fn security(inherit: bool) -> anyhow::Result<Security> {
    unsafe {
        let mut token = ptr::null_mut();
        ensure!(
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) != 0,
            "open current user token: {}",
            io::Error::last_os_error()
        );
        let token = Handle(token);
        let mut size = 0;
        GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut size);
        ensure!(size > 0 && size < 65536, "invalid user token size");
        let mut data = vec![
            0usize;
            (size as usize + std::mem::size_of::<usize>() - 1)
                / std::mem::size_of::<usize>()
        ];
        ensure!(
            GetTokenInformation(
                token.0,
                TokenUser,
                data.as_mut_ptr().cast(),
                size,
                &mut size
            ) != 0,
            "read current user token"
        );
        let user = &*data.as_ptr().cast::<TOKEN_USER>();
        let mut sid = ptr::null_mut();
        ensure!(
            ConvertSidToStringSidW(user.User.Sid, &mut sid) != 0,
            "format current user SID"
        );
        let mut len = 0;
        while *sid.add(len) != 0 {
            len += 1;
        }
        let sid_text = String::from_utf16(std::slice::from_raw_parts(sid, len))?;
        LocalFree(sid.cast());
        let flags = if inherit { "OICI" } else { "" };
        let sddl = wide(&format!(
            "D:P(A;{flags};GA;;;{sid_text})(A;{flags};GA;;;SY)"
        ));
        let mut descriptor = ptr::null_mut();
        ensure!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut()
            ) != 0,
            "create private pipe ACL"
        );
        Ok(Security(descriptor))
    }
}

fn create_handle(name: &str, first: bool) -> anyhow::Result<File> {
    let security = security(false)?;
    let attrs = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.0,
        bInheritHandle: 0,
    };
    let handle = unsafe {
        CreateNamedPipeW(
            wide(name).as_ptr(),
            PIPE_ACCESS_DUPLEX
                | FILE_FLAG_OVERLAPPED
                | if first {
                    FILE_FLAG_FIRST_PIPE_INSTANCE
                } else {
                    0
                },
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            65536,
            65536,
            5000,
            &attrs,
        )
    };
    ensure!(
        handle != INVALID_HANDLE_VALUE,
        "create local control pipe: {}",
        io::Error::last_os_error()
    );
    Ok(unsafe { File::from_raw_handle(handle.cast()) })
}

// A synchronous Windows file object serializes concurrent ReadFile/WriteFile,
// even through DuplicateHandle. The multiplexed console therefore uses genuine
// overlapped operations; a pending read must never block sending a new request.
struct Pipe {
    file: File,
    stop: OwnedHandle,
}
#[derive(Clone)]
pub(super) struct Duplex(Arc<Pipe>);
impl Duplex {
    fn new(file: File) -> io::Result<Self> {
        let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(Arc::new(Pipe {
            file,
            stop: unsafe { OwnedHandle::from_raw_handle(event.cast()) },
        })))
    }
    pub(super) fn cancel(&self) {
        unsafe {
            SetEvent(self.0.stop.as_raw_handle().cast());
            CancelIoEx(self.as_raw_handle().cast(), ptr::null());
        }
    }
}
impl AsRawHandle for Duplex {
    fn as_raw_handle(&self) -> RawHandle {
        self.0.file.as_raw_handle()
    }
}
pub(super) fn create_duplex(name: &str) -> anyhow::Result<Duplex> {
    create_control(name, true)
}
pub(super) fn create_control(name: &str, first: bool) -> anyhow::Result<Duplex> {
    Ok(Duplex::new(create_handle(name, first)?)?)
}
pub(super) fn connect_control(name: &str) -> anyhow::Result<Duplex> {
    use std::os::windows::fs::OpenOptionsExt;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_OVERLAPPED)
            .open(name)
        {
            Ok(file) => return Ok(Duplex::new(file)?),
            Err(error)
                if matches!(error.raw_os_error(), Some(2 | 231))
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            Err(error) => return Err(error).context("connect container control pipe"),
        }
    }
}
pub(super) fn accept_control(pipe: Duplex) -> io::Result<Duplex> {
    accept_duplex(pipe).map(|(reader, _)| reader)
}
pub(super) fn network_pair(name: &str) -> anyhow::Result<(File, File)> {
    use std::os::windows::fs::OpenOptionsExt;
    let server = create_handle(name, true)?;
    let client = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_OVERLAPPED)
        .open(name)?;
    let mut operation = Operation::new()?;
    let handle = server.as_raw_handle().cast();
    let ok = unsafe { ConnectNamedPipe(handle, &mut operation.overlapped) };
    if ok == 0 && io::Error::last_os_error().raw_os_error() != Some(ERROR_PIPE_CONNECTED as i32) {
        operation.finish(handle, ok)?;
    }
    Ok((server, client))
}
pub(super) fn private_directory(path: &Path) -> anyhow::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
    let security = security(true)?;
    let attrs = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.0,
        bInheritHandle: 0,
    };
    let path_wide = wide(path.to_str().context("network path must be UTF-8")?);
    if unsafe { CreateDirectoryW(path_wide.as_ptr(), &attrs) } == 0 {
        let error = io::Error::last_os_error();
        ensure!(
            error.kind() == io::ErrorKind::AlreadyExists,
            "create private network directory: {error}"
        );
    }
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "unsafe network directory"
    );
    Ok(())
}
pub(super) fn accept_duplex(pipe: Duplex) -> io::Result<(Duplex, Duplex)> {
    let mut operation = Operation::new()?;
    let handle = pipe.as_raw_handle().cast();
    let ok = unsafe { ConnectNamedPipe(handle, &mut operation.overlapped) };
    if ok == 0 && io::Error::last_os_error().raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) {
        return Ok((pipe.clone(), pipe));
    }
    operation.finish(handle, ok)?;
    Ok((pipe.clone(), pipe))
}
struct Operation {
    overlapped: OVERLAPPED,
    _event: Handle,
}
impl Operation {
    fn new() -> io::Result<Self> {
        let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            overlapped: OVERLAPPED {
                hEvent: event,
                ..Default::default()
            },
            _event: Handle(event),
        })
    }
    fn finish(&mut self, handle: HANDLE, ok: i32) -> io::Result<usize> {
        if ok == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(error);
            }
        }
        let mut transferred = 0;
        // Keep the operation, event and caller's buffer alive until completion.
        // No pending operation is abandoned with a pointer to stack storage.
        if unsafe { GetOverlappedResult(handle, &self.overlapped, &mut transferred, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(transferred as usize)
    }
    fn finish_cancelable(&mut self, pipe: &Duplex, ok: i32) -> io::Result<usize> {
        let handle = pipe.as_raw_handle().cast();
        if ok == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(error);
            }
            let events = [self.overlapped.hEvent, pipe.0.stop.as_raw_handle().cast()];
            let result = unsafe { WaitForMultipleObjects(2, events.as_ptr(), 0, u32::MAX) };
            if result != 0 {
                unsafe {
                    CancelIoEx(handle, &self.overlapped);
                }
                let mut transferred = 0;
                unsafe {
                    GetOverlappedResult(handle, &self.overlapped, &mut transferred, 1);
                }
                return Err(io::ErrorKind::ConnectionAborted.into());
            }
        }
        self.finish(handle, 1)
    }
}
impl Read for Duplex {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if unsafe {
            windows_sys::Win32::System::Threading::WaitForSingleObject(
                self.0.stop.as_raw_handle().cast(),
                0,
            )
        } == 0
        {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        if bytes.is_empty() {
            return Ok(0);
        }
        let mut operation = Operation::new()?;
        let handle = self.as_raw_handle().cast();
        let ok = unsafe {
            ReadFile(
                handle,
                bytes.as_mut_ptr(),
                bytes.len().min(u32::MAX as usize) as u32,
                ptr::null_mut(),
                &mut operation.overlapped,
            )
        };
        operation.finish_cancelable(self, ok)
    }
}
impl Write for Duplex {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if unsafe {
            windows_sys::Win32::System::Threading::WaitForSingleObject(
                self.0.stop.as_raw_handle().cast(),
                0,
            )
        } == 0
        {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        if bytes.is_empty() {
            return Ok(0);
        }
        let mut operation = Operation::new()?;
        let handle = self.as_raw_handle().cast();
        let ok = unsafe {
            WriteFile(
                handle,
                bytes.as_ptr(),
                bytes.len().min(u32::MAX as usize) as u32,
                ptr::null_mut(),
                &mut operation.overlapped,
            )
        };
        operation.finish_cancelable(self, ok)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn drain(file: &impl AsRawHandle) -> io::Result<()> {
    if unsafe { FlushFileBuffers(file.as_raw_handle().cast()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    #[test]
    fn cancellation_is_latched_and_releases_a_framed_read_without_peer_eof() {
        let name = format!(
            r"\\.\pipe\pvisor-cancel-test-{}",
            uuid::Uuid::new_v4().simple()
        );
        let server = create_duplex(&name).unwrap();
        let _open_client = connect_control(&name).unwrap();
        let mut server = accept_control(server).unwrap();
        let cancel = server.clone();
        let (done, finished) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let error =
                pvisor_guest::control::read_frame::<pvisor_guest::control::Reply>(&mut server)
                    .unwrap_err();
            done.send(error.kind()).unwrap();
        });
        cancel.cancel();
        assert_eq!(
            finished.recv_timeout(Duration::from_secs(5)).unwrap(),
            io::ErrorKind::ConnectionAborted
        );
        worker.join().unwrap();
        let mut second = cancel;
        assert_eq!(
            second.read(&mut [0; 1]).unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
    }

    #[test]
    fn network_pair_is_an_owned_byte_pipe_accepted_without_whp_startup() {
        use pvisor_vm::api::{NetworkStream, VmBuilder, VmConfiguration};
        let name = format!(
            r"\\.\pipe\pvisor-network-test-{}",
            uuid::Uuid::new_v4().simple()
        );
        let (_host, guest) = network_pair(&name).unwrap();
        let mut builder = VmBuilder::new(1, 128).unwrap();
        builder
            .network(NetworkStream::from(guest), [2, 0, 0, 0, 0, 2])
            .unwrap();
    }

    #[test]
    fn a_pending_duplex_read_does_not_serialize_the_opposite_write() {
        let name = format!(
            r"\\.\pipe\pvisor-duplex-test-{}",
            uuid::Uuid::new_v4().simple()
        );
        let pipe = create_duplex(&name).unwrap();
        let client = std::thread::spawn(move || {
            let mut client = connect_control(&name).unwrap();
            let mut byte = [0];
            client.read_exact(&mut byte).unwrap();
            assert_eq!(byte, [42]);
            client.write_all(&[7]).unwrap();
        });
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            let (reader, mut writer) = accept_duplex(pipe).unwrap();
            let handle = reader.as_raw_handle().cast();
            let mut byte = [0];
            let mut operation = Operation::new().unwrap();
            let ok = unsafe {
                ReadFile(
                    handle,
                    byte.as_mut_ptr(),
                    1,
                    ptr::null_mut(),
                    &mut operation.overlapped,
                )
            };
            assert_eq!(ok, 0);
            assert_eq!(
                io::Error::last_os_error().raw_os_error(),
                Some(ERROR_IO_PENDING as i32)
            );
            writer.write_all(&[42]).unwrap();
            let mut transferred = 0;
            assert_ne!(
                unsafe { GetOverlappedResult(handle, &operation.overlapped, &mut transferred, 1) },
                0
            );
            assert_eq!((transferred, byte), (1, [7]));
            done.send(()).unwrap();
        });
        finished.recv_timeout(Duration::from_secs(10)).unwrap();
        client.join().unwrap();
    }
}
