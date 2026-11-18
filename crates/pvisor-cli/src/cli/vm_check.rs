//! Owner-process VM preflight. Never boots a guest or changes host permissions.
#[derive(serde::Serialize)]
struct Report {
    schema_version: u32,
    operation: &'static str,
    platform: &'static str,
    architecture: &'static str,
    backend: &'static str,
    can_create_vm: bool,
    reason: String,
    firmware: &'static str,
    boot_verified: bool,
    scope: &'static str,
}

fn report() -> Report {
    let (backend, available, reason) = platform();
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    let firmware = {
        use pvisor_vm::api::{RuntimeSupport, VmPlatform};
        if VmPlatform::embedded_kernel().is_some() {
            "embedded"
        } else if VmPlatform::bundled_firmware_directory().is_some() {
            "bundled_unloaded"
        } else {
            "missing"
        }
    };
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    let firmware = "unsupported";
    Report {
        schema_version: 1,
        operation: "vm-check",
        platform: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        backend,
        can_create_vm: available,
        reason,
        firmware,
        boot_verified: false,
        scope: "Empty VM creation only; firmware loading, rootfs, staging, guest tools and workflow capabilities are not verified",
    }
}

pub(super) fn print(json: bool) -> anyhow::Result<()> {
    let result = report();
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "{}: {}; firmware: {}",
            result.backend, result.reason, result.firmware
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn platform() -> (&'static str, bool, String) {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let probe = (|| -> std::io::Result<()> {
        let device = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/kvm")?;
        // Linux KVM UAPI: GET_API_VERSION and CREATE_VM, machine type zero.
        let version = unsafe { libc::ioctl(device.as_raw_fd(), 0xAE00) };
        if version != 12 {
            return Err(std::io::Error::other(format!(
                "KVM API version {version}, expected 12"
            )));
        }
        let vm = unsafe { libc::ioctl(device.as_raw_fd(), 0xAE01, 0 as libc::c_ulong) };
        if vm < 0 {
            return Err(std::io::Error::last_os_error());
        }
        drop(unsafe { OwnedFd::from_raw_fd(vm) });
        Ok(())
    })();
    match probe {
        Ok(()) => (
            "kvm",
            true,
            "KVM_CREATE_VM succeeded for this process".into(),
        ),
        Err(error) => (
            "kvm",
            false,
            format!(
                "KVM unavailable to this process: {error}; check virtualization and /dev/kvm access, then restart the Agent host"
            ),
        ),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn platform() -> (&'static str, bool, String) {
    #[link(name = "Hypervisor", kind = "framework")]
    unsafe extern "C" {
        fn hv_vm_create(config: *const libc::c_void) -> i32;
        fn hv_vm_destroy() -> i32;
    }
    let result = unsafe { hv_vm_create(std::ptr::null()) };
    if result != 0 {
        return (
            "hvf",
            false,
            format!("HVF creation denied/unavailable: {result:#x}; check hypervisor entitlement"),
        );
    }
    let destroyed = unsafe { hv_vm_destroy() };
    (
        "hvf",
        destroyed == 0,
        if destroyed == 0 {
            "HVF empty VM creation and destruction succeeded".into()
        } else {
            format!("HVF destruction failed: {destroyed:#x}")
        },
    )
}

#[cfg(not(any(target_os = "linux", all(target_os = "macos", target_arch = "aarch64"))))]
fn platform() -> (&'static str, bool, String) {
    (
        "unsupported",
        false,
        "Plugin VM preflight is unsupported on this platform".into(),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn report_never_claims_a_booted_or_workflow_verified_guest() {
        let value = serde_json::to_value(super::report()).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["operation"], "vm-check");
        assert_eq!(value["boot_verified"], false);
        assert!(value["can_create_vm"].is_boolean());
        assert!(!value["reason"].as_str().unwrap().is_empty());
    }
}
