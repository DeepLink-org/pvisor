//! Inspect the actual CLI so shared libkrun dependencies cannot slip into builds.

use std::process::Command;

#[test]
fn pvisor_does_not_dynamically_link_libkrun() {
    #[cfg(target_os = "macos")]
    let (tool, flag) = ("otool", "-L");
    #[cfg(target_os = "linux")]
    let (tool, flag) = ("readelf", "-d");
    let output = Command::new(tool)
        .args([flag, env!("CARGO_BIN_EXE_pvisor")])
        .output()
        .expect("inspect pvisor's shared library dependencies");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let dependencies = String::from_utf8(output.stdout).unwrap();
    // Skip otool's first line (the executable path); readelf's first line is blank.
    for dependency in dependencies.lines().skip(1) {
        assert!(
            !dependency.contains("libkrun"),
            "pvisor must embed libkrun: {dependency}"
        );
    }
}
