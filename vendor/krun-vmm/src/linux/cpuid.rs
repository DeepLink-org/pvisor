//! Keep KVM-provided AMD boot mitigation features reachable after filtering.
use kvm_bindings::CpuId;

pub(crate) fn expose_supported_extended_leaves(cpuid: &mut CpuId) {
    // The pinned AMD transformer advertises a fixed 0x8000001f maximum. Newer
    // KVMs also supply 0x80000021 (AUTOIBRS, SRSO_USER_KERNEL_NO). Linux
    // checks the maximum before reading these leaves and otherwise selects
    // mitigations for older hardware. Raise the limit only through this leaf:
    // newer topology leaves need their own VM topology filtering. Never
    // synthesize feature bits or use unfiltered host CPUID.
    let amd = cpuid.as_slice().iter().any(|e| {
        e.function == 0 && e.ebx == 0x6874_7541 && e.edx == 0x6974_6e65 && e.ecx == 0x444d_4163
    });
    if !amd {
        return;
    }
    let mitigation_leaf = 0x8000_0021;
    let present = cpuid
        .as_slice()
        .iter()
        .any(|e| e.function == mitigation_leaf && e.index == 0);
    if present {
        if let Some(base) = cpuid
            .as_mut_slice()
            .iter_mut()
            .find(|e| e.function == 0x8000_0000)
        {
            base.eax = base.eax.max(mitigation_leaf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvm_bindings::kvm_cpuid_entry2;

    fn inventory(amd: bool, leaf: u32) -> CpuId {
        CpuId::from_entries(&[
            kvm_cpuid_entry2 {
                function: 0,
                ebx: if amd { 0x6874_7541 } else { 0x756e_6547 },
                edx: if amd { 0x6974_6e65 } else { 0x4965_6e69 },
                ecx: if amd { 0x444d_4163 } else { 0x6c65_746e },
                ..Default::default()
            },
            kvm_cpuid_entry2 {
                function: 0x8000_0000,
                eax: 0x8000_001f,
                ..Default::default()
            },
            kvm_cpuid_entry2 {
                function: leaf,
                eax: 0x1122,
                ebx: 0x3344,
                ecx: 0x5566,
                edx: 0x7788,
                ..Default::default()
            },
            // Hypervisor leaves must not contribute to the extended maximum.
            kvm_cpuid_entry2 {
                function: 0xc000_0010,
                ..Default::default()
            },
        ])
        .unwrap()
    }

    #[test]
    fn newer_amd_leaf_becomes_visible_without_changing_any_features() {
        let mut cpuid = inventory(true, 0x8000_0021);
        let mut expected = cpuid.clone();
        expected.as_mut_slice()[1].eax = 0x8000_0021;
        expose_supported_extended_leaves(&mut cpuid);
        assert_eq!(cpuid, expected);
    }

    #[test]
    fn older_amd_and_other_vendors_keep_their_existing_contract() {
        for (amd, leaf) in [
            (true, 0x8000_001e),
            (true, 0x8000_0026),
            (false, 0x8000_0021),
        ] {
            let mut cpuid = inventory(amd, leaf);
            let expected = cpuid.clone();
            expose_supported_extended_leaves(&mut cpuid);
            assert_eq!(cpuid, expected);
        }
        let mut cpuid = inventory(true, 0x8000_0021);
        cpuid.as_mut_slice()[1].eax = 0x8000_0028;
        let expected = cpuid.clone();
        expose_supported_extended_leaves(&mut cpuid);
        assert_eq!(cpuid, expected);
    }

    #[test]
    fn future_topology_leaves_do_not_expand_the_boot_feature_contract() {
        let mut entries = inventory(true, 0x8000_0021).as_slice().to_vec();
        entries.push(kvm_cpuid_entry2 {
            function: 0x8000_0026,
            eax: 0x1234,
            ..Default::default()
        });
        let mut cpuid = CpuId::from_entries(&entries).unwrap();
        let mut expected = cpuid.clone();
        expected.as_mut_slice()[1].eax = 0x8000_0021;
        expose_supported_extended_leaves(&mut cpuid);
        assert_eq!(cpuid, expected);
    }
}
