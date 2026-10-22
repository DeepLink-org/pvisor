//! Keep the optional control transport from becoming a second guest runtime.
#[test]
fn jobs_and_controlled_workloads_share_launch_and_reaping() {
    let boot = include_str!("../src/linux.rs");
    let control = include_str!("../src/linux/control.rs");
    assert!(!boot.contains("container::run"));
    assert!(!boot.contains("config.container"));
    for source in [boot, control] {
        for private_operation in [
            "libc::waitpid",
            "libc::waitid",
            "libc::kill",
            "libc::SYS_prlimit64",
        ] {
            assert!(
                !source.contains(private_operation),
                "supervision escaped the common launcher: {private_operation}"
            );
        }
    }
    assert!(!control.contains("Command::"));
    assert!(!control.contains("libc::setsid"));
    assert!(!control.contains("Docker"));
}
