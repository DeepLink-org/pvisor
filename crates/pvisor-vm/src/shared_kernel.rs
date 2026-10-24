//! Common immutable boot-kernel pages. No live-page scanning or VM pointers.
use std::{io, path::Path};

pub(crate) fn validate_directory(directory: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(directory)?;
    if !directory.is_absolute()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "shared kernel directory must be absolute, owned and private",
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn backing(directory: &Path, bytes: &[u8]) -> io::Result<std::sync::Arc<std::fs::File>> {
    use sha2::{Digest, Sha256};
    use std::{
        fs::{File, OpenOptions},
        io::Write,
        os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        sync::Arc,
    };
    validate_directory(directory)?;
    let id: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let path = directory.join(format!("kernel-v1-{id}"));
    if !path.try_exists()? {
        let mut pending = tempfile::NamedTempFile::new_in(directory)?;
        pending.write_all(bytes)?;
        pending.as_file().sync_all()?;
        pending
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o400))?;
        match pending.persist_noclobber(&path) {
            Ok(writer) => {
                drop(writer);
                File::open(directory)?.sync_all()?;
            }
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.error),
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o277 != 0
        || metadata.len() != bytes.len() as u64
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid shared kernel backing",
        ));
    }
    let mut buffer = [0; 64 * 1024];
    for (index, expected) in bytes.chunks(buffer.len()).enumerate() {
        file.read_exact_at(&mut buffer[..expected.len()], (index * 64 * 1024) as u64)?;
        if buffer[..expected.len()] != *expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "shared kernel content mismatch",
            ));
        }
    }
    Ok(Arc::new(file))
}

/// Construct private COW kernel RAM and ordinary RAM around it before HVF sees
/// any pointers. FileOffset retains the exact backing, including after unlink.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn map(
    ranges: &[(vm_memory::GuestAddress, usize)],
    ordinary_backing: Option<std::sync::Arc<std::fs::File>>,
    directory: &Path,
    address: u64,
    bytes: &[u8],
) -> io::Result<vm_memory::GuestMemoryMmap> {
    use std::sync::Arc;
    use vm_memory::{
        Address, FileOffset, GuestAddress, GuestMemoryMmap, GuestRegionMmap,
        mmap::MmapRegionBuilder,
    };
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page <= 0
        || bytes.is_empty()
        || !address.is_multiple_of(page as u64)
        || !bytes.len().is_multiple_of(page as usize)
    {
        return Err(invalid("shared kernel must be host-page aligned"));
    }
    let end = address
        .checked_add(bytes.len() as u64)
        .ok_or_else(|| invalid("kernel address overflow"))?;
    let mut total = 0u64;
    let mut last = 0u64;
    let mut contains = false;
    for &(base, size) in ranges {
        let start = base.raw_value();
        let limit = start
            .checked_add(size as u64)
            .ok_or_else(|| invalid("RAM address overflow"))?;
        if size == 0
            || start < last
            || !start.is_multiple_of(page as u64)
            || !size.is_multiple_of(page as usize)
        {
            return Err(invalid("invalid shared kernel RAM geometry"));
        }
        contains |= start <= address && end <= limit;
        last = limit;
        total = total
            .checked_add(size as u64)
            .ok_or_else(|| invalid("RAM length overflow"))?;
    }
    if !contains {
        return Err(invalid("shared kernel is outside ordinary RAM"));
    }
    let kernel = backing(directory, bytes)?;
    if let Some(file) = &ordinary_backing {
        file.set_len(total)?;
    }
    let mut regions = Vec::new();
    let mut offset = 0;
    for &(base, size) in ranges {
        let start = base.raw_value();
        let limit = start + size as u64;
        let spans = if start <= address && end <= limit {
            vec![
                (start, address, false),
                (address, end, true),
                (end, limit, false),
            ]
        } else {
            vec![(start, limit, false)]
        };
        for (first, last, shared) in spans {
            if first == last {
                continue;
            }
            let mut builder = MmapRegionBuilder::new((last - first) as usize)
                .with_mmap_prot(libc::PROT_READ | libc::PROT_WRITE);
            if shared {
                builder = builder
                    .with_file_offset(FileOffset::from_arc(kernel.clone(), 0))
                    .with_mmap_flags(libc::MAP_PRIVATE);
            } else if let Some(file) = &ordinary_backing {
                builder = builder
                    .with_file_offset(FileOffset::from_arc(file.clone(), offset + first - start))
                    .with_mmap_flags(libc::MAP_SHARED);
            } else {
                builder = builder.with_mmap_flags(libc::MAP_PRIVATE | libc::MAP_ANONYMOUS);
            }
            let mapped = builder.build().map_err(io::Error::other)?;
            regions.push(Arc::new(
                GuestRegionMmap::new(mapped, GuestAddress(first))
                    .ok_or_else(|| invalid("invalid shared kernel mapping"))?,
            ));
        }
        offset += size as u64;
    }
    GuestMemoryMmap::from_arc_regions(regions).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use vm_memory::{Bytes, GuestAddress, GuestMemory};

    #[test]
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[ignore = "requires signed HVF test binary and local process permissions"]
    fn hvf_two_processes_share_baseline_and_isolate_cpu_and_device_writes() {
        use std::{
            io::{BufRead, BufReader, Write},
            process::{Child, Command, Stdio},
        };
        const DIRECTORY: &str = "PVISOR_SHARED_KERNEL_TEST_DIRECTORY";
        const CHILD: &str = "PVISOR_SHARED_KERNEL_TEST_CHILD";
        if let Some(directory) = std::env::var_os(DIRECTORY) {
            use crate::hvf::{HvfVcpu, HvfVm, bindings::*};
            use vm_memory::GuestMemoryMmap;
            let directory = std::path::PathBuf::from(directory);
            let value: u64 = std::env::var(CHILD).unwrap().parse().unwrap();
            let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
            let original = vec![0x51; page * 4];
            let memory = map(
                &[(GuestAddress(0x200000), page * 4)],
                None,
                &directory,
                0x200000,
                &original,
            )
            .unwrap();
            let code =
                GuestMemoryMmap::<()>::from_ranges(&[(GuestAddress(0x100000), page)]).unwrap();
            let words = [0xf9400023u32, 0xd4000002, 0xf9000022, 0xd4000002]; // LDR/HVC, STR/HVC.
            for (index, word) in words.iter().enumerate() {
                code.write_slice(
                    &word.to_le_bytes(),
                    GuestAddress(0x100000 + index as u64 * 4),
                )
                .unwrap();
            }
            let code_host = code.get_host_address(GuestAddress(0x100000)).unwrap();
            unsafe extern "C" {
                fn sys_icache_invalidate(address: *mut libc::c_void, size: usize);
            }
            unsafe {
                sys_icache_invalidate(code_host.cast(), 16);
            }
            let vm = HvfVm::new(false).unwrap();
            vm.map_memory(code_host as u64, 0x100000, page as u64)
                .unwrap();
            let host = memory.get_host_address(GuestAddress(0x200000)).unwrap();
            vm.map_memory(host as u64, 0x200000, (page * 4) as u64)
                .unwrap();
            let cpu = HvfVcpu::new(0, false).unwrap();
            cpu.set_initial_state(0x100000, 0).unwrap();
            let guest_read = |address| {
                cpu.write_reg(hv_reg_t_HV_REG_PC, 0x100000).unwrap();
                cpu.write_reg(hv_reg_t_HV_REG_X1, address).unwrap();
                assert_eq!(unsafe { hv_vcpu_run(cpu.id()) }, HV_SUCCESS);
                let mut read = 0;
                assert_eq!(
                    unsafe { hv_vcpu_get_reg(cpu.id(), hv_reg_t_HV_REG_X3, &mut read) },
                    HV_SUCCESS
                );
                read
            };
            for index in 0..3 {
                assert_eq!(
                    guest_read(0x200000 + (page * index) as u64),
                    0x5151515151515151
                );
            }
            let identity = || crate::vmm::ram::page_inventory(&memory).unwrap().1[2];
            println!(
                "{}",
                serde_json::json!({"phase": "ready", "page": identity()})
            );
            std::io::stdout().flush().unwrap();
            for command in std::io::stdin().lock().lines() {
                match command.unwrap().as_str() {
                    "write" => {
                        cpu.write_reg(hv_reg_t_HV_REG_PC, 0x100008).unwrap();
                        cpu.write_reg(hv_reg_t_HV_REG_X1, 0x200000).unwrap();
                        cpu.write_reg(hv_reg_t_HV_REG_X2, value).unwrap();
                        assert_eq!(unsafe { hv_vcpu_run(cpu.id()) }, HV_SUCCESS);
                        let mut result = [0; 8];
                        memory
                            .read_slice(&mut result, GuestAddress(0x200000))
                            .unwrap();
                        assert_eq!(u64::from_le_bytes(result), value);
                        // Device workers write through this same owned GuestMemory mapping.
                        memory
                            .write_slice(&value.to_le_bytes(), GuestAddress(0x200000 + page as u64))
                            .unwrap();
                        assert_eq!(
                            guest_read(0x200000 + page as u64),
                            value,
                            "guest must observe a device write after host COW"
                        );
                        backing(&directory, &original).unwrap();
                    }
                    "verify" => {
                        for address in [0x200000, 0x200000 + page as u64] {
                            let mut result = [0; 8];
                            memory
                                .read_slice(&mut result, GuestAddress(address))
                                .unwrap();
                            assert_eq!(u64::from_le_bytes(result), value);
                            assert_eq!(guest_read(address), value);
                        }
                    }
                    "exit" => break,
                    other => panic!("unexpected test command: {other}"),
                }
                println!(
                    "{}",
                    serde_json::json!({"phase": "done", "page": identity()})
                );
                std::io::stdout().flush().unwrap();
            }
            assert_eq!(unsafe { hv_vcpu_destroy(cpu.id()) }, HV_SUCCESS);
            vm.unmap_memory(0x100000, page as u64).unwrap();
            vm.unmap_memory(0x200000, (page * 4) as u64).unwrap();
            assert_eq!(unsafe { hv_vm_destroy() }, HV_SUCCESS);
            return;
        }
        struct Process {
            child: Child,
            output: std::sync::mpsc::Receiver<String>,
        }
        impl Process {
            fn read(&mut self) -> serde_json::Value {
                loop {
                    let line = self
                        .output
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .expect("HVF child exited or stopped responding");
                    if let Some(start) = line.find('{') {
                        if let Ok(value) = serde_json::from_str(&line[start..]) {
                            return value;
                        }
                    }
                }
            }
            fn send(&mut self, command: &str) -> serde_json::Value {
                writeln!(self.child.stdin.as_mut().unwrap(), "{command}").unwrap();
                self.child.stdin.as_mut().unwrap().flush().unwrap();
                self.read()
            }
        }
        impl Drop for Process {
            fn drop(&mut self) {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let spawn = |value: u64| {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "shared_kernel::tests::hvf_two_processes_share_baseline_and_isolate_cpu_and_device_writes", "--ignored", "--nocapture"])
                .env(DIRECTORY, directory.path()).env(CHILD, value.to_string())
                .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
            let reader = BufReader::new(child.stdout.take().unwrap());
            let (sender, output) = std::sync::mpsc::sync_channel(4);
            std::thread::spawn(move || {
                for line in reader.lines() {
                    let Ok(line) = line else {
                        break;
                    };
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            });
            Process { child, output }
        };
        let (mut a, mut b) = (spawn(0xa1), spawn(0xb2));
        let (first, second) = (a.read(), b.read());
        // Mach object/offset identities are not PFNs or a net-memory metric.
        assert_eq!(first["phase"], "ready");
        assert_eq!(second["phase"], "ready");
        assert!(first["page"][1].as_u64().unwrap() > 0);
        assert_eq!(first["page"][1], second["page"][1]);
        assert_eq!(first["page"][2], second["page"][2]);
        let (aw, bw) = (a.send("write"), b.send("write"));
        assert_eq!(aw["page"][1], bw["page"][1]);
        assert_eq!(aw["page"][2], bw["page"][2]);
        a.send("verify");
        b.send("verify");
        writeln!(a.child.stdin.as_mut().unwrap(), "exit").unwrap();
        assert!(a.child.wait().unwrap().success());
        let path = std::fs::read_dir(directory.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::remove_file(path).unwrap();
        b.send("verify");
    }

    #[test]
    fn common_inode_maps_privately_preserves_device_writes_and_survives_unlink() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let bytes = vec![0x51; page * 2];
        let ranges = [(GuestAddress(0), page * 4)];
        let a = map(&ranges, None, directory.path(), page as u64, &bytes).unwrap();
        let b = map(&ranges, None, directory.path(), page as u64, &bytes).unwrap();
        let af = a.iter().find_map(|region| region.file_offset()).unwrap();
        let bf = b.iter().find_map(|region| region.file_offset()).unwrap();
        assert_eq!(
            af.file().metadata().unwrap().ino(),
            bf.file().metadata().unwrap().ino()
        );
        a.write_slice(b"device-a", GuestAddress(page as u64))
            .unwrap();
        b.write_slice(b"device-b", GuestAddress(page as u64))
            .unwrap();
        let mut result = [0; 8];
        a.read_slice(&mut result, GuestAddress(page as u64))
            .unwrap();
        assert_eq!(&result, b"device-a");
        b.read_slice(&mut result, GuestAddress(page as u64))
            .unwrap();
        assert_eq!(&result, b"device-b");
        use std::os::unix::fs::FileExt;
        af.file().read_exact_at(&mut result, 0).unwrap();
        assert_eq!(result, [0x51; 8]);
        let path = std::fs::read_dir(directory.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::remove_file(path).unwrap();
        drop(a);
        b.read_slice(&mut result, GuestAddress((page * 2) as u64))
            .unwrap();
        assert_eq!(result, [0x51; 8]);
    }

    #[test]
    fn invalid_geometry_and_corrupt_cache_fail_without_altering_live_baseline() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let bytes = vec![7; page];
        assert!(
            map(
                &[(GuestAddress(0), page)],
                None,
                directory.path(),
                1,
                &bytes
            )
            .is_err()
        );
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .next()
                .is_none()
        );
        let _file = backing(directory.path(), &bytes).unwrap();
        let path = std::fs::read_dir(directory.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, vec![8; page]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        assert_eq!(
            backing(directory.path(), &bytes).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn cold_tokens_exclude_immutable_kernel_and_offload_refuses_private_cow() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let file = std::sync::Arc::new(tempfile::tempfile().unwrap());
        let memory = map(
            &[(GuestAddress(0), page * 4)],
            Some(file),
            directory.path(),
            page as u64,
            &vec![0x51; page * 2],
        )
        .unwrap();
        let blocks = crate::vmm::ram::blocks(&memory, page).unwrap();
        let addresses: Vec<_> = blocks.iter().map(|block| block.guest_address()).collect();
        assert_eq!(addresses, [0, (page * 3) as u64]);
        assert!(crate::vmm::ram::check_reclaim(&memory).is_err());
        let ordinary = crate::vmm::ram::map(
            &[(GuestAddress(0), page * 3)],
            &[],
            std::sync::Arc::new(tempfile::tempfile().unwrap()),
        )
        .unwrap();
        assert!(
            crate::vmm::ram::blocks(&ordinary, page * 2).is_err(),
            "ordinary RAM retains strict complete-chunk validation"
        );
    }
}
