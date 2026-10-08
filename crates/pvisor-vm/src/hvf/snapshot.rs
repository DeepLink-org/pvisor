//! Stopped-vCPU capture/restore. This is a building block, not a VM snapshot:
//! the caller must also preserve RAM, interrupt controllers and devices.
use super::{bindings::*, Error, HvfVcpu, MmioRead};
use serde::{Deserialize, Serialize};

// Declare the native dependency here as well as in build.rs so internal
// hardware probe targets link the SIMD shim without importing the public crate.
#[link(name = "krun_snapshot_simd", kind = "static")]
extern "C" {
    fn krun_snapshot_get_q(cpu: hv_vcpu_t, reg: u32, out: *mut u8) -> hv_return_t;
    fn krun_snapshot_set_q(cpu: hv_vcpu_t, reg: u32, input: *const u8) -> hv_return_t;
}

const FEATURES: &[u16] = &[
    hv_sys_reg_t_HV_SYS_REG_MIDR_EL1,
    hv_sys_reg_t_HV_SYS_REG_MPIDR_EL1,
    hv_sys_reg_t_HV_SYS_REG_ID_AA64PFR0_EL1,
    hv_sys_reg_t_HV_SYS_REG_ID_AA64PFR1_EL1,
    hv_sys_reg_t_HV_SYS_REG_ID_AA64DFR0_EL1,
    hv_sys_reg_t_HV_SYS_REG_ID_AA64DFR1_EL1,
    hv_sys_reg_t_HV_SYS_REG_ID_AA64ISAR0_EL1,
    hv_sys_reg_t_HV_SYS_REG_ID_AA64ISAR1_EL1,
    hv_sys_reg_t_HV_SYS_REG_ID_AA64MMFR0_EL1,
    hv_sys_reg_t_HV_SYS_REG_ID_AA64MMFR1_EL1,
    hv_sys_reg_t_HV_SYS_REG_ID_AA64MMFR2_EL1,
];
// Explicit architectural list; CNT*_TVAL aliases CVAL and must not be restored.
// Physical timers are not exposed by the non-nested HVF/libkrun profile.
// SCTLR is last so translation is enabled only after its prerequisites.
const SYSTEM: &[u16] = &[
    hv_sys_reg_t_HV_SYS_REG_ACTLR_EL1,
    hv_sys_reg_t_HV_SYS_REG_CPACR_EL1,
    hv_sys_reg_t_HV_SYS_REG_TTBR0_EL1,
    hv_sys_reg_t_HV_SYS_REG_TTBR1_EL1,
    hv_sys_reg_t_HV_SYS_REG_TCR_EL1,
    hv_sys_reg_t_HV_SYS_REG_SPSR_EL1,
    hv_sys_reg_t_HV_SYS_REG_ELR_EL1,
    hv_sys_reg_t_HV_SYS_REG_SP_EL0,
    hv_sys_reg_t_HV_SYS_REG_SP_EL1,
    hv_sys_reg_t_HV_SYS_REG_AFSR0_EL1,
    hv_sys_reg_t_HV_SYS_REG_AFSR1_EL1,
    hv_sys_reg_t_HV_SYS_REG_ESR_EL1,
    hv_sys_reg_t_HV_SYS_REG_FAR_EL1,
    hv_sys_reg_t_HV_SYS_REG_PAR_EL1,
    hv_sys_reg_t_HV_SYS_REG_MAIR_EL1,
    hv_sys_reg_t_HV_SYS_REG_AMAIR_EL1,
    hv_sys_reg_t_HV_SYS_REG_VBAR_EL1,
    hv_sys_reg_t_HV_SYS_REG_CONTEXTIDR_EL1,
    hv_sys_reg_t_HV_SYS_REG_TPIDR_EL1,
    hv_sys_reg_t_HV_SYS_REG_TPIDR_EL0,
    hv_sys_reg_t_HV_SYS_REG_TPIDRRO_EL0,
    hv_sys_reg_t_HV_SYS_REG_CNTKCTL_EL1,
    hv_sys_reg_t_HV_SYS_REG_CSSELR_EL1,
    hv_sys_reg_t_HV_SYS_REG_MDCCINT_EL1,
    hv_sys_reg_t_HV_SYS_REG_MDSCR_EL1,
    hv_sys_reg_t_HV_SYS_REG_CNTV_CVAL_EL0,
    hv_sys_reg_t_HV_SYS_REG_CNTV_CTL_EL0,
    hv_sys_reg_t_HV_SYS_REG_SCTLR_EL1,
];
const PAC: &[u16] = &[
    hv_sys_reg_t_HV_SYS_REG_APIAKEYLO_EL1,
    hv_sys_reg_t_HV_SYS_REG_APIAKEYHI_EL1,
    hv_sys_reg_t_HV_SYS_REG_APIBKEYLO_EL1,
    hv_sys_reg_t_HV_SYS_REG_APIBKEYHI_EL1,
    hv_sys_reg_t_HV_SYS_REG_APDAKEYLO_EL1,
    hv_sys_reg_t_HV_SYS_REG_APDAKEYHI_EL1,
    hv_sys_reg_t_HV_SYS_REG_APDBKEYLO_EL1,
    hv_sys_reg_t_HV_SYS_REG_APDBKEYHI_EL1,
    hv_sys_reg_t_HV_SYS_REG_APGAKEYLO_EL1,
    hv_sys_reg_t_HV_SYS_REG_APGAKEYHI_EL1,
];

/// Fixed-schema state contains no pointers, host descriptors or HVF CPU IDs.
/// Fields are private; deserialized values must pass validation before restore.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VcpuSnapshot {
    version: u32,
    frequency: u64,
    features: Vec<(u16, u64)>,
    registers: Vec<u64>,
    simd: [[u8; 16]; 32],
    system: Vec<(u16, u64)>,
    timer_offset: u64,
    timer_mask: bool,
    pending_irq: bool,
    pending_fiq: bool,
    trap_debug_exceptions: bool,
    trap_debug_registers: bool,
    mmio_read: Option<MmioRead>,
    mmio_buf: [u8; 8],
    advance_pc: bool,
    wrapper_timer_mask: bool,
}

fn checked(ret: hv_return_t, operation: &str) -> Result<(), Error> {
    if ret == HV_SUCCESS {
        Ok(())
    } else {
        Err(Error::Snapshot(format!("{operation}: HVF status {ret:#x}")))
    }
}
fn read_system(cpu: hv_vcpu_t, reg: u16) -> Result<u64, Error> {
    let mut value = 0;
    checked(
        unsafe { hv_vcpu_get_sys_reg(cpu, reg, &mut value) },
        &format!("read system register {reg:#x}"),
    )?;
    Ok(value)
}
fn system_list(features: &[(u16, u64)]) -> Result<Vec<u16>, Error> {
    if features.len() != FEATURES.len() || features.iter().map(|p| p.0).ne(FEATURES.iter().copied())
    {
        return Err(Error::Snapshot("invalid feature register list".into()));
    }
    let value = |reg| features.iter().find(|p| p.0 == reg).unwrap().1;
    if (value(hv_sys_reg_t_HV_SYS_REG_ID_AA64PFR0_EL1) >> 32) & 15 != 0
        || value(hv_sys_reg_t_HV_SYS_REG_ID_AA64PFR1_EL1) & 0x0f00_0f00 != 0
    {
        return Err(Error::Snapshot(format!(
            "SVE, SME and MTE are not supported: PFR0={:#x} PFR1={:#x}",
            value(hv_sys_reg_t_HV_SYS_REG_ID_AA64PFR0_EL1),
            value(hv_sys_reg_t_HV_SYS_REG_ID_AA64PFR1_EL1)
        )));
    }
    let debug = value(hv_sys_reg_t_HV_SYS_REG_ID_AA64DFR0_EL1);
    let mut list = Vec::new();
    for (base, count) in [
        (
            hv_sys_reg_t_HV_SYS_REG_DBGBVR0_EL1,
            ((debug >> 12) & 15) + 1,
        ),
        (
            hv_sys_reg_t_HV_SYS_REG_DBGWVR0_EL1,
            ((debug >> 20) & 15) + 1,
        ),
    ] {
        for i in 0..count as u16 {
            list.extend([base + 8 * i, base + 8 * i + 1]);
        }
    }
    if value(hv_sys_reg_t_HV_SYS_REG_ID_AA64ISAR1_EL1) & 0xff00_0ff0 != 0 {
        list.extend_from_slice(PAC);
    }
    list.extend_from_slice(SYSTEM);
    Ok(list)
}

impl VcpuSnapshot {
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1 || self.frequency == 0 || self.registers.len() != 35 {
            return Err(Error::Snapshot(
                "invalid vCPU schema, frequency or registers".into(),
            ));
        }
        if self
            .system
            .iter()
            .map(|p| p.0)
            .ne(system_list(&self.features)?)
        {
            return Err(Error::Snapshot("invalid system register list".into()));
        }
        if let Some(read) = &self.mmio_read {
            if !matches!(read.len, 1 | 2 | 4 | 8) || read.srt > 31 || !self.advance_pc {
                return Err(Error::Snapshot("invalid deferred MMIO read".into()));
            }
        }
        Ok(())
    }
}

impl HvfVcpu<'_> {
    /// Create the restricted non-nested CPU profile for cold-restore experiments.
    /// Hide extensions whose state this snapshot schema cannot preserve, before
    /// the guest executes any instruction. Existing VM CPU profiles are unchanged.
    pub fn new_snapshot_cpu(mpidr: u64) -> Result<Self, Error> {
        let cpu = Self::new(mpidr, false)?;
        let configure = || -> Result<(), Error> {
            for (reg, mask) in [
                (hv_sys_reg_t_HV_SYS_REG_ID_AA64PFR0_EL1, 0xf_u64 << 32),
                (hv_sys_reg_t_HV_SYS_REG_ID_AA64PFR1_EL1, 0x0f00_0f00),
            ] {
                let old = read_system(cpu.vcpuid, reg)?;
                if old & mask != 0 {
                    checked(
                        unsafe { hv_vcpu_set_sys_reg(cpu.vcpuid, reg, old & !mask) },
                        "disable unsupported CPU extension",
                    )?;
                    if read_system(cpu.vcpuid, reg)? != old & !mask {
                        return Err(Error::Snapshot("CPU extension mask was not applied".into()));
                    }
                }
            }
            Ok(())
        };
        if let Err(error) = configure() {
            checked(
                unsafe { hv_vcpu_destroy(cpu.vcpuid) },
                "destroy rejected snapshot CPU",
            )?;
            return Err(error);
        }
        Ok(cpu)
    }

    /// Call on the owning vCPU thread, after run() returned. The device owner
    /// must have completed the last MMIO exit before calling this method.
    pub fn capture_state(&self) -> Result<VcpuSnapshot, Error> {
        if self.nested_enabled {
            return Err(Error::Snapshot(
                "nested virtualization is not supported".into(),
            ));
        }
        let cpu = self.vcpuid;
        let features = FEATURES
            .iter()
            .map(|&r| Ok((r, read_system(cpu, r)?)))
            .collect::<Result<Vec<_>, Error>>()?;
        let system = system_list(&features)?
            .into_iter()
            .map(|r| Ok((r, read_system(cpu, r)?)))
            .collect::<Result<Vec<_>, Error>>()?;
        let registers = (0..=hv_reg_t_HV_REG_CPSR)
            .map(|r| self.read_reg(r))
            .collect::<Result<Vec<_>, _>>()?;
        let mut simd = [[0; 16]; 32];
        for (i, q) in simd.iter_mut().enumerate() {
            checked(
                unsafe { krun_snapshot_get_q(cpu, i as u32, q.as_mut_ptr()) },
                "read SIMD register",
            )?;
        }
        let mut timer_offset = 0;
        let mut timer_mask = false;
        let mut pending_irq = false;
        let mut pending_fiq = false;
        let mut trap_debug_exceptions = false;
        let mut trap_debug_registers = false;
        checked(
            unsafe { hv_vcpu_get_vtimer_offset(cpu, &mut timer_offset) },
            "read timer offset",
        )?;
        checked(
            unsafe { hv_vcpu_get_vtimer_mask(cpu, &mut timer_mask) },
            "read timer mask",
        )?;
        checked(
            unsafe {
                hv_vcpu_get_pending_interrupt(
                    cpu,
                    hv_interrupt_type_t_HV_INTERRUPT_TYPE_IRQ,
                    &mut pending_irq,
                )
            },
            "read pending IRQ",
        )?;
        checked(
            unsafe {
                hv_vcpu_get_pending_interrupt(
                    cpu,
                    hv_interrupt_type_t_HV_INTERRUPT_TYPE_FIQ,
                    &mut pending_fiq,
                )
            },
            "read pending FIQ",
        )?;
        checked(
            unsafe { hv_vcpu_get_trap_debug_exceptions(cpu, &mut trap_debug_exceptions) },
            "read debug traps",
        )?;
        checked(
            unsafe { hv_vcpu_get_trap_debug_reg_accesses(cpu, &mut trap_debug_registers) },
            "read debug register traps",
        )?;
        let state = VcpuSnapshot {
            version: 1,
            frequency: self.cntfrq,
            features,
            registers,
            simd,
            system,
            timer_offset,
            timer_mask,
            pending_irq,
            pending_fiq,
            trap_debug_exceptions,
            trap_debug_registers,
            mmio_read: self.pending_mmio_read.clone(),
            mmio_buf: self.mmio_buf,
            advance_pc: self.pending_advance_pc,
            wrapper_timer_mask: self.vtimer_masked,
        };
        state.validate()?;
        Ok(state)
    }

    /// Install on a new, stopped vCPU on its owning thread. After any error,
    /// discard that destination vCPU: partial installation is not resumable.
    pub fn restore_state(&mut self, state: &VcpuSnapshot) -> Result<(), Error> {
        state.validate()?;
        if self.nested_enabled || self.cntfrq != state.frequency {
            return Err(Error::Snapshot(
                "incompatible nested mode or counter frequency".into(),
            ));
        }
        for &(r, v) in &state.features {
            if read_system(self.vcpuid, r)? != v {
                return Err(Error::Snapshot(format!("CPU feature mismatch at {r:#x}")));
            }
        }
        let cpu = self.vcpuid;
        checked(
            unsafe { hv_vcpu_set_vtimer_mask(cpu, true) },
            "mask timer during restore",
        )?;
        for &(r, v) in &state.system {
            checked(
                unsafe { hv_vcpu_set_sys_reg(cpu, r, v) },
                &format!("restore system register {r:#x}"),
            )?;
        }
        for (i, q) in state.simd.iter().enumerate() {
            checked(
                unsafe { krun_snapshot_set_q(cpu, i as u32, q.as_ptr()) },
                "restore SIMD register",
            )?;
        }
        for (r, &v) in state.registers.iter().enumerate() {
            self.write_reg(r as u32, v)?;
        }
        checked(
            unsafe { hv_vcpu_set_vtimer_offset(cpu, state.timer_offset) },
            "restore timer offset",
        )?;
        checked(
            unsafe { hv_vcpu_set_trap_debug_exceptions(cpu, state.trap_debug_exceptions) },
            "restore debug traps",
        )?;
        checked(
            unsafe { hv_vcpu_set_trap_debug_reg_accesses(cpu, state.trap_debug_registers) },
            "restore debug register traps",
        )?;
        checked(
            unsafe {
                hv_vcpu_set_pending_interrupt(
                    cpu,
                    hv_interrupt_type_t_HV_INTERRUPT_TYPE_IRQ,
                    state.pending_irq,
                )
            },
            "restore IRQ",
        )?;
        checked(
            unsafe {
                hv_vcpu_set_pending_interrupt(
                    cpu,
                    hv_interrupt_type_t_HV_INTERRUPT_TYPE_FIQ,
                    state.pending_fiq,
                )
            },
            "restore FIQ",
        )?;
        checked(
            unsafe { hv_vcpu_set_vtimer_mask(cpu, state.timer_mask) },
            "restore timer mask",
        )?;
        self.pending_mmio_read = state.mmio_read.clone();
        self.mmio_buf = state.mmio_buf;
        self.pending_advance_pc = state.advance_pc;
        self.vtimer_masked = state.wrapper_timer_mask;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> VcpuSnapshot {
        let features: Vec<_> = FEATURES.iter().map(|&r| (r, 0)).collect();
        let system = system_list(&features)
            .unwrap()
            .into_iter()
            .map(|r| (r, 0))
            .collect();
        VcpuSnapshot {
            version: 1,
            frequency: 24_000_000,
            features,
            registers: vec![0; 35],
            simd: [[0; 16]; 32],
            system,
            timer_offset: 0,
            timer_mask: false,
            pending_irq: false,
            pending_fiq: false,
            trap_debug_exceptions: false,
            trap_debug_registers: false,
            mmio_read: None,
            mmio_buf: [0; 8],
            advance_pc: false,
            wrapper_timer_mask: false,
        }
    }

    #[test]
    fn schema_and_register_lists_are_closed() {
        let original = state();
        original.validate().unwrap();
        let mut bad = original.clone();
        bad.version += 1;
        assert!(bad.validate().is_err());
        bad = original.clone();
        bad.registers.pop();
        assert!(bad.validate().is_err());
        bad = original.clone();
        bad.system.reverse();
        assert!(bad.validate().is_err());
        bad = original.clone();
        bad.features.swap(0, 1);
        assert!(bad.validate().is_err());
        bad = original;
        bad.system[0].0 = 0xffff;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn unsupported_extensions_are_rejected() {
        for (index, mask) in [(2, 1_u64 << 32), (3, 1 << 24), (3, 1 << 8)] {
            let mut bad = state();
            bad.features[index].1 |= mask;
            assert!(bad.validate().is_err());
        }
    }

    #[test]
    fn deferred_mmio_must_have_one_valid_completion() {
        let mut state = state();
        state.mmio_read = Some(MmioRead {
            addr: 0x300000,
            len: 8,
            srt: 2,
        });
        assert!(state.validate().is_err());
        state.advance_pc = true;
        state.validate().unwrap();
        state.mmio_read.as_mut().unwrap().len = 3;
        assert!(state.validate().is_err());
        state.mmio_read.as_mut().unwrap().len = 8;
        state.mmio_read.as_mut().unwrap().srt = 32;
        assert!(state.validate().is_err());
    }
}
