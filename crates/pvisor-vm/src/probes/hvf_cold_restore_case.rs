#[macro_use]
extern crate log;
include!("../runtime_modules.rs");

// Persist a stopped HVF CPU and RAM, exit, restore in a fresh process.
// Not a Linux/pVisor VM snapshot: no GIC, virtio or filesystem devices.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn main() -> anyhow::Result<()> {
    use crate::hvf::{bindings::*, snapshot::VcpuSnapshot, HvfVcpu, HvfVm, VcpuExit, Vcpus};
    use anyhow::{ensure, Context};
    use serde::{Deserialize, Serialize};
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::OpenOptionsExt;
    use std::{io::Write, sync::Arc};

    struct Interrupts;
    impl Vcpus for Interrupts {
        fn set_vtimer_irq(&self, _: u64) {}
        fn should_wait(&self, _: u64) -> bool {
            false
        }
        fn has_pending_irq(&self, _: u64) -> bool {
            false
        }
        fn get_pending_irq(&self, _: u64) -> u32 {
            1023
        }
        fn handle_sysreg_read(&self, _: u64, _: u32) -> Option<u64> {
            None
        }
        fn handle_sysreg_write(&self, _: u64, _: u32, _: u64) -> bool {
            false
        }
    }
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Image {
        source_pid: u32,
        page: usize,
        cpu: VcpuSnapshot,
        ram: Vec<u8>,
        ram_digest: [u8; 32],
    }
    let mut args = std::env::args().skip(1);
    let mode = args.next().context("expected save or restore")?;
    let path = args.next().context("expected snapshot file")?;
    ensure!(args.next().is_none(), "unexpected argument");
    ensure!(mode == "save" || mode == "restore", "unknown mode");
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    ensure!((4096..=65536).contains(&page), "invalid page size");
    let saved = if mode == "restore" {
        let file = std::fs::File::open(&path)?;
        ensure!(file.metadata()?.len() <= 1024 * 1024, "oversized snapshot");
        let image: Image = serde_json::from_reader(file)?;
        image
            .cpu
            .validate()
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        ensure!(
            image.page == page && image.ram.len() == page,
            "invalid RAM layout"
        );
        ensure!(
            <[u8; 32]>::from(Sha256::digest(&image.ram)) == image.ram_digest,
            "RAM checksum mismatch"
        );
        ensure!(
            image.source_pid != std::process::id(),
            "restore must use a new process"
        );
        Some(image)
    } else {
        None
    };
    // Process-local anonymous RAM is intentionally independent of the source.
    let ram = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            page,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    ensure!(ram != libc::MAP_FAILED, "RAM mmap failed");
    let bytes = unsafe { std::slice::from_raw_parts_mut(ram.cast::<u8>(), page) };
    if let Some(image) = &saved {
        bytes.copy_from_slice(&image.ram);
    } else {
        // MMIO LDR; increment x9; store x9; shutdown via PSCI. Capturing after
        // LDR must preserve its completed read and advance PC exactly once.
        let code = [
            0xf9400022_u32,
            0x91000529,
            0xf9000069,
            0x3d800080,
            0xd503207f,
            0xd4000002,
        ];
        for (i, word) in code.iter().enumerate() {
            bytes[4 * i..4 * i + 4].copy_from_slice(&word.to_le_bytes());
        }
    }
    unsafe extern "C" {
        fn sys_icache_invalidate(address: *mut libc::c_void, size: usize);
    }
    unsafe {
        sys_icache_invalidate(ram, page);
    }
    let vm = HvfVm::new(false).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    vm.map_memory(ram as u64, 0x100000, page as u64)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let mut cpu = HvfVcpu::new_snapshot_cpu(0).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let interrupts = Arc::new(Interrupts);
    if let Some(image) = saved {
        cpu.restore_state(&image.cpu)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        match cpu
            .run(interrupts.clone())
            .map_err(|e| anyhow::anyhow!(e.to_string()))?
        {
            VcpuExit::WaitForEventTimeout(timeout) => {
                ensure!(
                    timeout > std::time::Duration::from_secs(5)
                        && timeout <= std::time::Duration::from_secs(30),
                    "restored WFE used wrong timer clock: {timeout:?}"
                );
            }
            exit => anyhow::bail!("unexpected restored timer exit: {exit:?}"),
        }
        match cpu
            .run(interrupts)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?
        {
            VcpuExit::Shutdown => (),
            exit => anyhow::bail!("unexpected restored exit: {exit:?}"),
        }
        let mut x2 = 0;
        ensure!(
            unsafe { hv_vcpu_get_reg(cpu.id(), hv_reg_t_HV_REG_X2, &mut x2) } == HV_SUCCESS,
            "read x2"
        );
        ensure!(x2 == 0x123456789abcdef0, "deferred MMIO value lost");
        let counter = u64::from_le_bytes(bytes[1024..1032].try_into()?);
        ensure!(counter == 42, "execution duplicated or RAM lost: {counter}");
        ensure!(
            bytes[2048..2064] == (0..16).collect::<Vec<u8>>(),
            "guest SIMD execution lost Q0"
        );
        // Recapture exercises SIMD C vector ABI in both directions.
        let roundtrip = cpu
            .capture_state()
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let before = serde_json::to_value(&image.cpu)?;
        let after = serde_json::to_value(roundtrip)?;
        // Architectural control values can change through execution, so verify
        // only untouched SIMD and EL1 software context, not all dynamic state.
        ensure!(before["simd"] == after["simd"], "SIMD state lost");
        for (reg, (saved, restored)) in before["registers"]
            .as_array()
            .unwrap()
            .iter()
            .zip(after["registers"].as_array().unwrap())
            .enumerate()
        {
            if ![hv_reg_t_HV_REG_X2, hv_reg_t_HV_REG_X9, hv_reg_t_HV_REG_PC].contains(&(reg as u32))
            {
                ensure!(saved == restored, "untouched register {reg} changed");
            }
        }
        let differences: Vec<_> = before["system"]
            .as_array()
            .unwrap()
            .iter()
            .zip(after["system"].as_array().unwrap())
            .filter(|(a, b)| a != b)
            .collect();
        ensure!(
            differences.is_empty(),
            "EL1 system state changed: {differences:?}"
        );
        ensure!(
            before["timer_offset"] == after["timer_offset"],
            "timer offset lost"
        );
        println!(
            "restored source_pid={} destination_pid={} counter={counter} mmio_reads=0",
            image.source_pid,
            std::process::id()
        );
    } else {
        cpu.set_initial_state(0x100000, 0x84000008)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        for (reg, value) in [
            (hv_reg_t_HV_REG_X1, 0x300000),
            (hv_reg_t_HV_REG_X3, 0x100400),
            (hv_reg_t_HV_REG_X9, 41),
            (hv_reg_t_HV_REG_X4, 0x100800),
        ] {
            cpu.write_reg(reg, value)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        }
        match cpu
            .run(interrupts)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?
        {
            VcpuExit::MmioRead(address, data) => {
                ensure!(
                    address == 0x300000 && data.len() == 8,
                    "unexpected MMIO read"
                );
                data.copy_from_slice(&0x123456789abcdef0_u64.to_le_bytes());
            }
            exit => anyhow::bail!("unexpected capture exit: {exit:?}"),
        }
        let captured = cpu
            .capture_state()
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let mut value = serde_json::to_value(captured)?;
        for (i, q) in value["simd"].as_array_mut().unwrap().iter_mut().enumerate() {
            *q = serde_json::json!((0..16).map(|j| (i * 7 + j) as u8).collect::<Vec<_>>());
        }
        let frequency = value["frequency"].as_u64().context("missing frequency")?;
        let offset = frequency.checked_mul(60).context("timer offset overflow")?;
        let compare = unsafe { crate::hvf::mach_absolute_time() }
            .wrapping_sub(offset)
            .wrapping_add(frequency * 30);
        for (reg, data) in [
            (hv_sys_reg_t_HV_SYS_REG_CPACR_EL1, 3_u64 << 20),
            (hv_sys_reg_t_HV_SYS_REG_TPIDR_EL0, 0x1020304050607080_u64),
            (hv_sys_reg_t_HV_SYS_REG_CNTV_CVAL_EL0, compare),
            (hv_sys_reg_t_HV_SYS_REG_CNTV_CTL_EL0, 1_u64),
        ] {
            let pair = value["system"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|pair| pair[0].as_u64() == Some(reg as u64))
                .unwrap();
            pair[1] = serde_json::json!(data);
        }
        // HVF canonicalizes disabled breakpoint BAS fields to all four bytes
        // on first execution. Seed that canonical value to compare exact state.
        for pair in value["system"].as_array_mut().unwrap() {
            let reg = pair[0].as_u64().unwrap() as u16;
            if (hv_sys_reg_t_HV_SYS_REG_DBGBCR0_EL1..=hv_sys_reg_t_HV_SYS_REG_DBGBCR15_EL1)
                .contains(&reg)
                && (reg - hv_sys_reg_t_HV_SYS_REG_DBGBCR0_EL1).is_multiple_of(8)
            {
                pair[1] = serde_json::json!(0x1e0_u64);
            }
        }
        value["timer_offset"] = serde_json::json!(offset);
        value["registers"][hv_reg_t_HV_REG_FPCR as usize] = serde_json::json!(1_u64 << 22);
        value["registers"][hv_reg_t_HV_REG_FPSR as usize] = serde_json::json!(1_u64 << 4);
        // Install nonzero SIMD through the production restore path, then
        // capture it through the production get path before publishing.
        let seed: VcpuSnapshot = serde_json::from_value(value)?;
        cpu.restore_state(&seed)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let verified = cpu
            .capture_state()
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        ensure!(
            serde_json::to_value(&seed)?["simd"] == serde_json::to_value(&verified)?["simd"],
            "SIMD ABI roundtrip failed"
        );
        let image = Image {
            source_pid: std::process::id(),
            page,
            cpu: verified,
            ram: bytes.to_vec(),
            ram_digest: Sha256::digest(&*bytes).into(),
        };
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(&serde_json::to_vec(&image)?)?;
        file.sync_all()?;
        println!("saved source_pid={} mmio_reads=1", std::process::id());
    }
    Ok(())
}
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn main() {
    eprintln!("requires macOS Apple Silicon");
    std::process::exit(1);
}
