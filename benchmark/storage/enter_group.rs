//! B-NATIVE-DOCKER-ENG: enter the experiment's delegated leaf, then exec argv.
//! Both subjects use this same frozen launcher; no daemon or timing loop lives here.
use std::os::unix::process::CommandExt;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let group = std::path::PathBuf::from(args.next().expect("delegated cgroup"));
    assert!(group.starts_with("/sys/fs/cgroup/user.slice"));
    std::fs::write(group.join("cgroup.procs"), std::process::id().to_string())
        .expect("enter owned delegated leaf");
    let executable = args.next().expect("subject executable");
    panic!(
        "exec: {}",
        std::process::Command::new(executable).args(args).exec()
    );
}
