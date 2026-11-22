use std::{env, path::PathBuf};

pub(super) fn external_firmware() -> Option<PathBuf> {
    let firmware = env::var_os("PVISOR_CASE_VM_FIRMWARE_PATH");
    if env::var("PVISOR_GUEST_KERNEL").as_deref() == Ok("rust-native") {
        assert!(
            firmware.is_none(),
            "Rust native firmware is embedded; unset PVISOR_CASE_VM_FIRMWARE_PATH"
        );
        None
    } else {
        Some(
            firmware
                .expect("set an absolute trusted local firmware path")
                .into(),
        )
    }
}
