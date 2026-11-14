use anyhow::{Context, ensure};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::io::AsRawHandle,
    path::Path,
    ptr,
};
use windows_sys::Win32::System::{IO::DeviceIoControl, Ioctl::FSCTL_SET_SPARSE};

pub(super) fn create(path: &Path) -> anyhow::Result<()> {
    let template = std::env::current_exe()?
        .parent()
        .context("executable directory")?
        .join("pvisor-rootfs.bin.gz");
    let input = File::open(&template).with_context(|| {
        format!(
            "installation is missing ext4 template {}",
            template.display()
        )
    })?;
    let mut decoder = flate2::read::GzDecoder::new(input).take((64 << 20) + 1);
    let mut magic = [0; 8];
    decoder.read_exact(&mut magic)?;
    ensure!(&magic == b"PVROOT1\0", "invalid ext4 template format");
    let mut u64buf = [0; 8];
    decoder.read_exact(&mut u64buf)?;
    let size = u64::from_le_bytes(u64buf);
    ensure!(size == 4u64 << 30, "unsupported ext4 template geometry");
    let mut file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)?;
    let mut returned = 0;
    ensure!(
        unsafe {
            DeviceIoControl(
                file.as_raw_handle().cast(),
                FSCTL_SET_SPARSE,
                ptr::null(),
                0,
                ptr::null_mut(),
                0,
                &mut returned,
                ptr::null_mut(),
            )
        } != 0,
        "container storage must support sparse files: {}",
        std::io::Error::last_os_error()
    );
    file.set_len(size)?;
    let mut end = 0;
    let mut total = 0;
    loop {
        decoder.read_exact(&mut u64buf)?;
        let offset = u64::from_le_bytes(u64buf);
        let mut len = [0; 4];
        decoder.read_exact(&mut len)?;
        let len = u32::from_le_bytes(len) as u64;
        if offset == u64::MAX {
            ensure!(len == 0, "invalid template end");
            break;
        }
        ensure!(
            offset >= end
                && len > 0
                && len <= 1 << 20
                && offset.checked_add(len).is_some_and(|e| e <= size),
            "invalid ext4 template extent"
        );
        total += len;
        ensure!(total <= 60 << 20, "ext4 template metadata exceeds 60 MiB");
        let mut data = vec![0; len as usize];
        decoder.read_exact(&mut data)?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(&data)?;
        end = offset + len;
    }
    let mut remaining = [0; 1];
    ensure!(
        decoder.read(&mut remaining)? == 0,
        "trailing ext4 template bytes"
    );
    file.sync_all()?;
    Ok(())
}
