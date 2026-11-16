use pvisor_overlay_core::{
    OverlayCore, OverlayLayout,
    stage::{self, StageDurability},
};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn sealed_gate_rejects_special_logs_without_blocking_or_repair() {
    const CHILD: &str = "PVISOR_PREIMAGE_TYPE_CHILD";
    if let Some(journal) = std::env::var_os(CHILD) {
        let error = stage::require_sealed(Path::new(&journal)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        return;
    }
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let lower = root.join("lower");
    let upper = root.join("upper");
    let journal = root.join("journal");
    fs::create_dir(&lower).unwrap();
    stage::begin(&journal, StageDurability::Strict).unwrap();
    let core = OverlayCore::new_for_layout_with_compact_preimages(
        OverlayLayout::new(vec![lower.clone()], lower).unwrap(),
        upper.clone(),
        Some(root.join("work")),
        vec![],
        Some(journal.clone()),
    )
    .unwrap();
    drop(core);
    stage::seal(&upper, &journal).unwrap();
    stage::require_sealed(&journal).unwrap();
    let log = journal.join("log-v2");
    let original = fs::read(&log).unwrap();
    fs::remove_file(&log).unwrap();
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(log.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sealed_gate_rejects_special_logs_without_blocking_or_repair",
            "--nocapture",
        ])
        .env(CHILD, &journal)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("sealed gate blocked on a FIFO without a writer");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success());
    use std::os::unix::fs::FileTypeExt;
    assert!(fs::symlink_metadata(&log).unwrap().file_type().is_fifo());
    fs::remove_file(&log).unwrap();
    fs::create_dir(&log).unwrap();
    assert!(stage::require_sealed(&journal).is_err());
    fs::remove_dir(&log).unwrap();
    let outside = root.join("outside");
    fs::write(&outside, &original).unwrap();
    std::os::unix::fs::symlink(&outside, &log).unwrap();
    assert!(stage::require_sealed(&journal).is_err());
    assert_eq!(fs::read(&outside).unwrap(), original);
    fs::remove_file(&log).unwrap();
    fs::write(&log, b"broken").unwrap();
    assert!(stage::require_sealed(&journal).is_err());
    assert_eq!(fs::read(&log).unwrap(), b"broken");
    fs::write(&log, &original).unwrap();
    stage::require_sealed(&journal).unwrap();
}
