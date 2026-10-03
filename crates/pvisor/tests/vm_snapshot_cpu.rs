//! Reject missing supervisor XSAVE state before entering KVM, without hardware.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[path = "../../../vendor/krun-vmm/src/linux/cpuid.rs"]
mod cpuid;
use krun_vmm::CpuSnapshot;
use kvm_bindings::{
    CpuId, Msrs, Xsave, kvm_cpuid_entry2, kvm_debugregs, kvm_lapic_state, kvm_mp_state,
    kvm_msr_entry, kvm_regs, kvm_sregs, kvm_vcpu_events, kvm_xcrs,
};

fn cpu(supervisor: u32, xss: bool) -> CpuSnapshot {
    let mut msrs = vec![kvm_msr_entry {
        index: 0x10,
        ..Default::default()
    }];
    if xss {
        msrs.push(kvm_msr_entry {
            index: 0xda0,
            data: 0x800,
            ..Default::default()
        });
    }
    serde_json::from_value(serde_json::json!({
        "id": 0,
        "cpu": {
            "cpuid": CpuId::from_entries(&[kvm_cpuid_entry2 {
                function: 0xd, index: 1, ecx: supervisor, ..Default::default()
            }]).unwrap(),
            "msrs": Msrs::from_entries(&msrs).unwrap(),
            "debug_regs": kvm_debugregs::default(),
            "lapic": kvm_lapic_state::default(),
            "mp_state": kvm_mp_state::default(),
            "regs": kvm_regs::default(),
            "sregs": kvm_sregs::default(),
            "vcpu_events": kvm_vcpu_events::default(),
            "xcrs": kvm_xcrs::default(),
            "xsave": Xsave::new(0).unwrap(),
        }
    }))
    .unwrap()
}

#[test]
fn supervisor_xsave_requires_xss_but_legacy_cpu_does_not() {
    cpu(0, false).validate(0).unwrap();
    cpu(0x800, true).validate(0).unwrap();
    assert!(
        cpu(0x800, false)
            .validate(0)
            .unwrap_err()
            .contains("IA32_XSS")
    );
    assert!(cpu(0x800, true).validate(1).is_err());
}
