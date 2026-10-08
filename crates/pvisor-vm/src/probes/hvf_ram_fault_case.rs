#[macro_use]
extern crate log;
include!("../runtime_modules.rs");

// Real HVF validation of the vendored RAM-fault hook. No Linux guest/devices.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn main() -> anyhow::Result<()> {
    use crate::hvf::{bindings::*, HvfVcpu, HvfVm, MemoryFault, VcpuExit, Vcpus};
    use anyhow::{ensure, Context};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    struct Resolver {
        vm: Arc<HvfVm>,
        mode: String,
        calls: AtomicUsize,
        page: u64,
    }
    impl Vcpus for Resolver {
        fn handle_memory_fault(&self, fault: MemoryFault) -> Result<bool, String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let address = if self.mode == "fetch" {
                0x100000
            } else {
                0x200000
            };
            if fault.guest_address != address {
                return Ok(false);
            }
            match self.mode.as_str() {
                "reject" => Ok(false),
                "error" => Err("injected recovery failure".into()),
                _ => {
                    let expected = if self.mode == "fetch" { 0x20 } else { 0x24 };
                    if fault.syndrome >> 26 != expected {
                        return Err("unexpected fault class".into());
                    }
                    self.vm
                        .protect_memory(
                            address,
                            self.page,
                            (HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC) as u64,
                        )
                        .map_err(|e| e.to_string())?;
                    Ok(true)
                }
            }
        }
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
    struct Mapping(*mut libc::c_void, usize);
    impl Mapping {
        fn new(size: usize) -> anyhow::Result<Self> {
            // SAFETY: fresh private mapping, never exposed outside this single-thread probe.
            let address = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    size,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_ANON | libc::MAP_PRIVATE,
                    -1,
                    0,
                )
            };
            ensure!(address != libc::MAP_FAILED, "mmap failed");
            Ok(Self(address, size))
        }
    }
    impl Drop for Mapping {
        fn drop(&mut self) {
            unsafe {
                libc::munmap(self.0, self.1);
            }
        }
    }
    let mode = std::env::args()
        .nth(1)
        .context("expected retry, reject, error or fetch")?;
    ensure!(
        ["retry", "reject", "error", "fetch"].contains(&mode.as_str()),
        "unknown mode"
    );
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    ensure!(page >= 4096, "invalid host page size");
    let code = Mapping::new(page)?;
    let data = Mapping::new(page)?;
    // str x2,[x1]; hvc #0. x0 selects PSCI shutdown; RAM faults retry STR.
    let words = [0xf9000022u32, 0xd4000002];
    unsafe {
        std::ptr::copy_nonoverlapping(words.as_ptr(), code.0.cast::<u32>(), words.len());
        unsafe extern "C" {
            fn sys_icache_invalidate(address: *mut libc::c_void, size: usize);
        }
        sys_icache_invalidate(code.0, size_of_val(&words));
    }
    let vm = Arc::new(HvfVm::new(false).map_err(|e| anyhow::anyhow!(e.to_string()))?);
    vm.map_memory(code.0 as u64, 0x100000, page as u64)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    vm.map_memory(data.0 as u64, 0x200000, page as u64)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let target = if mode == "fetch" { 0x100000 } else { 0x200000 };
    vm.protect_memory(target, page as u64, HV_MEMORY_READ as u64)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let resolver = Arc::new(Resolver {
        vm: vm.clone(),
        mode: mode.clone(),
        calls: AtomicUsize::new(0),
        page: page as u64,
    });
    let mut cpu = HvfVcpu::new(0, false).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    cpu.set_initial_state(0x100000, 0x84000008)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    cpu.write_reg(hv_reg_t_HV_REG_X1, 0x200000)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    cpu.write_reg(hv_reg_t_HV_REG_X2, 123)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    match cpu.run(resolver.clone()) {
        Ok(VcpuExit::MemoryFaultHandled) => {
            ensure!(mode == "retry" || mode == "fetch", "unexpected recovery")
        }
        Ok(VcpuExit::MmioWrite(address, bytes)) => {
            ensure!(
                mode == "reject" && address == 0x200000 && bytes == 123u64.to_le_bytes(),
                "MMIO changed"
            );
        }
        Err(crate::hvf::Error::MemoryFault(error)) => ensure!(
            mode == "error" && error == "injected recovery failure",
            "wrong error"
        ),
        other => anyhow::bail!("unexpected exit: {other:?}"),
    }
    let mut pc = 0;
    ensure!(
        unsafe { hv_vcpu_get_reg(cpu.id(), hv_reg_t_HV_REG_PC, &mut pc) } == HV_SUCCESS,
        "read PC failed"
    );
    ensure!(pc == 0x100000, "fault prematurely advanced PC");
    ensure!(
        resolver.calls.load(Ordering::Relaxed) == 1,
        "handler count changed"
    );
    if mode == "retry" || mode == "fetch" {
        ensure!(
            matches!(cpu.run(resolver.clone()), Ok(VcpuExit::Shutdown)),
            "retry did not reach shutdown"
        );
        ensure!(
            unsafe { *data.0.cast::<u64>() } == 123,
            "faulting store was skipped"
        );
    } else {
        ensure!(
            unsafe { *data.0.cast::<u64>() } == 0,
            "failed/unowned fault modified RAM"
        );
    }
    ensure!(
        unsafe { hv_vcpu_destroy(cpu.id()) } == HV_SUCCESS,
        "destroy vCPU failed"
    );
    vm.unmap_memory(0x100000, page as u64)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    vm.unmap_memory(0x200000, page as u64)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    ensure!(
        unsafe { hv_vm_destroy() } == HV_SUCCESS,
        "destroy VM failed"
    );
    println!(
        "hvf-ram-fault-{mode}: passed; original PC preserved, retry and MMIO ownership checked"
    );
    Ok(())
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn main() {
    eprintln!("Requires Apple silicon macOS");
    std::process::exit(2);
}
