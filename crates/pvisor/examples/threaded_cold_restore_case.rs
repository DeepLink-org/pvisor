//! Owning-thread CPU/RAM/software-GIC check. No Linux or virtio devices.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use devices::legacy::{GicSnapshot, GicV3, VcpuList};
    use krun_vmm::{CpuSnapshot, Vcpu, VcpuEvent, VcpuHandle, VcpuResponse, Vm};
    use serde::{Deserialize, Serialize};
    use sha2::{Digest, Sha256};
    use std::{fs::OpenOptions, io::Write, sync::Arc, time::Duration};
    use utils::eventfd::{EFD_NONBLOCK, EventFd};
    use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};

    const GPA: u64 = 0x4000_0000;
    const TIMEOUT: Duration = Duration::from_secs(3);
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Image {
        version: u32,
        source_pid: u32,
        ram: Vec<u8>,
        ram_sha256: [u8; 32],
        secondary: String,
        cpus: Vec<CpuSnapshot>,
        gic: GicSnapshot,
    }
    fn pause(cpu: &VcpuHandle) -> Result<(), Box<dyn std::error::Error>> {
        cpu.send_event(VcpuEvent::Pause)?;
        if cpu.response_receiver().recv_timeout(TIMEOUT)? != VcpuResponse::Paused {
            return Err("unexpected pause response".into());
        }
        Ok(())
    }
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 || !["save", "restore"].contains(&args[1].as_str()) {
        return Err("expected save|restore PATH".into());
    }
    let restore = args[1] == "restore";
    let image = if restore {
        let bytes = std::fs::read(&args[2])?;
        if bytes.len() > 1024 * 1024 {
            return Err("image too large".into());
        }
        let image: Image = serde_json::from_slice(&bytes)?;
        if image.version != 1
            || image.source_pid == std::process::id()
            || image.ram.len() != 16384
            || image.cpus.len() != 2
        {
            return Err("invalid image".into());
        }
        for (i, state) in image.cpus.iter().enumerate() {
            state.validate(i as u8)?;
        }
        if <[u8; 32]>::from(Sha256::digest(&image.ram)) != image.ram_sha256 {
            return Err("RAM checksum mismatch".into());
        }
        if !image.cpus[0].booted
            || !["waiting", "running", "pending"].contains(&image.secondary.as_str())
            || image.cpus[1].booted != (image.secondary == "running")
            || image.cpus[1].pending_boot
                != if image.secondary == "pending" {
                    Some(GPA + 64)
                } else {
                    None
                }
        {
            return Err("unexpected PSCI state".into());
        }
        Some(image)
    } else {
        None
    };
    let secondary = image
        .as_ref()
        .map(|image| image.secondary.clone())
        .unwrap_or_else(|| {
            std::env::var("PVISOR_CASE_SECONDARY").unwrap_or_else(|_| "waiting".into())
        });
    if !["waiting", "running", "pending"].contains(&secondary.as_str()) {
        return Err("unknown secondary mode".into());
    }
    let mut vm = Vm::new(false)?;
    let memory = GuestMemoryMmap::from_ranges(&[(GuestAddress(GPA), 16384)])?;
    if let Some(image) = &image {
        memory.write_slice(&image.ram, GuestAddress(GPA))?;
    } else {
        // Each CPU increments its own counter then WFI. Restored PC resumes
        // after WFI, rather than rerunning initialization at the entry address.
        for cpu in 0..2 {
            for (i, word) in [
                0xd2a8_0001_u32,
                0xf942_0029 + cpu * 0x20000,
                0x9100_0529,
                0xf902_0029 + cpu * 0x20000,
                0xd503_207f,
                0x17ff_fffc,
            ]
            .iter()
            .enumerate()
            {
                memory.write_slice(
                    &word.to_le_bytes(),
                    GuestAddress(GPA + cpu as u64 * 64 + i as u64 * 4),
                )?;
            }
        }
    }
    vm.memory_init(&memory)?;
    let list = Arc::new(VcpuList::new(2));
    let mut gic = GicV3::new(list.clone());
    if let Some(image) = &image {
        gic.restore_state(&image.gic)?;
    }
    let (boot_sender, boot_receiver) = crossbeam_channel::unbounded();
    let mut handles = Vec::new();
    for id in 0..2 {
        let mut cpu = Vcpu::new_aarch64(
            id,
            GuestAddress(GPA),
            if id == 1 {
                Some(boot_receiver.clone())
            } else {
                None
            },
            EventFd::new(EFD_NONBLOCK)?,
            list.clone(),
            false,
        )?;
        cpu.enable_snapshot_profile()?;
        if let Some(image) = &image {
            cpu.set_restore_state(image.cpus[id as usize].clone())?;
        }
        let handle = cpu.start_threaded()?;
        if restore && handle.response_receiver().recv_timeout(TIMEOUT)? != VcpuResponse::Paused {
            return Err("restored CPU did not park".into());
        }
        handles.push(handle);
    }
    if !restore && secondary == "running" {
        boot_sender.send(GPA + 64)?;
    }
    if restore {
        // No instruction may run before the caller explicitly resumes all CPUs.
        assert_eq!(memory.read_obj::<u64>(GuestAddress(GPA + 1024))?, 1);
        for cpu in &handles {
            cpu.send_event(VcpuEvent::Resume)?;
        }
        for cpu in &handles {
            assert_eq!(
                cpu.response_receiver().recv_timeout(TIMEOUT)?,
                VcpuResponse::Resumed
            );
        }
    }
    let expected = if restore { 2 } else { 1 };
    let deadline = std::time::Instant::now() + TIMEOUT;
    let secondary_expected = match secondary.as_str() {
        "running" => expected,
        "pending" if restore => 1,
        _ => 0,
    };
    while memory.read_obj::<u64>(GuestAddress(GPA + 1024))? != expected
        || memory.read_obj::<u64>(GuestAddress(GPA + 2048))? != secondary_expected
    {
        if std::time::Instant::now() >= deadline {
            return Err("counter did not continue".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    for cpu in &handles {
        pause(cpu)?;
    }
    if !restore && secondary == "pending" {
        boot_sender.send(GPA + 64)?;
    }
    let cpus = handles
        .iter()
        .map(|cpu| cpu.capture_state(TIMEOUT))
        .collect::<Result<Vec<_>, _>>()?;
    assert!(cpus[0].booted);
    assert_eq!(
        cpus[1].booted,
        secondary == "running" || (secondary == "pending" && restore)
    );
    assert_eq!(
        cpus[1].pending_boot,
        if secondary == "pending" && !restore {
            Some(GPA + 64)
        } else {
            None
        }
    );
    if !restore {
        let mut ram = vec![0; 16384];
        memory.read_slice(&mut ram, GuestAddress(GPA))?;
        let image = Image {
            version: 1,
            source_pid: std::process::id(),
            ram_sha256: Sha256::digest(&ram).into(),
            ram,
            secondary: secondary.clone(),
            cpus,
            gic: gic.capture_state()?,
        };
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&args[2])?;
        file.write_all(&serde_json::to_vec(&image)?)?;
        file.sync_all()?;
    }
    println!(
        "{} counter={expected} secondary_counter={secondary_expected} cpus=2 secondary={secondary} pid={}",
        args[1],
        std::process::id()
    );
    Ok(())
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn main() {
    eprintln!("requires Apple Silicon macOS");
    std::process::exit(1);
}
