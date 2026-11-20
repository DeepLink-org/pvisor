use super::super::supported::{RAW_LEASE_CHILD_FD, runner_fd};
use super::check_raw_pager;
use crate::environment_snapshot::{Compatibility, SnapshotStore};
use pvisor_vm::api::{MachineRestore, RamRestoreExtent, RestoreState};
use std::{
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            net::{UnixListener, UnixStream},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::Arc,
    time::{Duration, Instant},
};
use vm_memory::{Bytes, GuestAddress};

const TEST: &str =
    "executor::vm::restore_ram::compact_fuse_fixture::compact_raw_fuse_survives_supervisor_loss";
const ROLE: &str = "PVISOR_TEST_COMPACT_FUSE_ROLE";
const CONTROL: &str = "PVISOR_TEST_COMPACT_FUSE_CONTROL";
const CLI: &str = "PVISOR_TEST_COMPACT_FUSE_CLI";
const RAM_FD: i32 = 200;
const BLOCK: usize = crate::ram_backing::BLOCK_BYTES;
const RAM_BYTES: usize = 4 * BLOCK;
const LIMIT: Duration = Duration::from_secs(15);

fn compatibility() -> Compatibility {
    Compatibility {
        host_boot: "fixture-boot".into(),
        build: "fixture-build".into(),
        firmware: "fixture-firmware".into(),
        profile: "compact-fuse-lease".into(),
    }
}

fn command(role: &str, control: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--ignored", "--exact", TEST, "--nocapture"])
        .env(ROLE, role)
        .env(CONTROL, control);
    command
}

fn configure(stream: &UnixStream) {
    stream.set_read_timeout(Some(LIMIT)).unwrap();
    stream.set_write_timeout(Some(LIMIT)).unwrap();
}

fn byte(stream: &mut impl Read) -> u8 {
    let mut value = [0];
    stream.read_exact(&mut value).unwrap();
    value[0]
}

fn await_condition(description: &str, check: impl FnMut() -> bool) {
    let deadline = Instant::now() + LIMIT;
    let mut check = check;
    while !check() {
        assert!(Instant::now() < deadline, "waiting for {description}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().is_ok_and(|status| status.is_none()) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn owner(control: &Path) {
    let directory = control.parent().unwrap();
    let store = SnapshotStore::new(&directory.join("store")).unwrap();
    let id = fs::read_to_string(directory.join("snapshot")).unwrap();
    let published = store.open(&id, &compatibility()).unwrap();
    let mut reader = published.ram_reader().unwrap();
    let extents = reader.compact_zero_restore().unwrap();
    assert_eq!(extents.len(), 4);
    assert_eq!(extents[0].backing_offset, None);
    assert_eq!(extents[1].backing_offset, Some(0));
    assert_eq!(extents[2].backing_offset, None);
    assert_eq!(extents[3].backing_offset, Some(BLOCK as u64));
    fs::write(
        directory.join("extents"),
        serde_json::to_vec(&extents).unwrap(),
    )
    .unwrap();

    let cli = fs::canonicalize(std::env::var_os(CLI).unwrap()).unwrap();
    let (mount, file) = published
        .raw_runtime_ram_mount(&cli, directory, &reader)
        .unwrap();
    let fd = file.as_raw_fd();
    let mounted_file = fs::read_link(format!("/proc/self/fd/{fd}")).unwrap();
    let mountpoint = mounted_file.parent().unwrap();
    let info = fs::read_to_string(format!("/proc/self/fdinfo/{fd}")).unwrap();
    let mount_id = info
        .lines()
        .find_map(|line| line.strip_prefix("mnt_id:"))
        .unwrap()
        .trim()
        .parse::<u64>()
        .unwrap();
    let mut receipt = UnixStream::connect(control).unwrap();
    configure(&receipt);
    receipt.write_all(b"o").unwrap();
    serde_json::to_writer(&mut receipt, &(mount_id, mountpoint)).unwrap();
    receipt.write_all(b"\n").unwrap();

    let _runners = ["a", "b"].map(|role| {
        let mut command = command(role, control);
        let ram = runner_fd(file.as_raw_fd()).unwrap();
        let lease = runner_fd(mount.runner_lease().unwrap().as_raw_fd()).unwrap();
        let ram_fd = ram.as_raw_fd();
        let lease_fd = lease.as_raw_fd();
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(ram_fd, RAM_FD) < 0 || libc::dup2(lease_fd, RAW_LEASE_CHILD_FD) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        OwnedChild(command.spawn().unwrap())
    });
    let _ = receipt.read(&mut [0]);
}

fn consumer(control: &Path, role: u8) {
    let file = unsafe { File::from_raw_fd(RAM_FD) };
    let lease = unsafe { OwnedFd::from_raw_fd(RAW_LEASE_CHILD_FD) };
    let extents: Vec<RamRestoreExtent> =
        serde_json::from_slice(&fs::read(control.parent().unwrap().join("extents")).unwrap())
            .unwrap();
    let restore = MachineRestore {
        ram_extents: extents,
        ram_file: Arc::new(file),
        state: serde_json::from_value(serde_json::json!({
            "version": 1, "cpus": [], "devices": [],
            "ram": [{"base": 0, "len": RAM_BYTES, "file_offset": 0}],
        }))
        .unwrap(),
    };
    let memory = restore.map_ram(&[(GuestAddress(0), RAM_BYTES)]).unwrap();
    let private = GuestAddress(BLOCK as u64);
    assert_eq!(memory.read_obj::<u8>(GuestAddress(0)).unwrap(), 0);
    assert_eq!(memory.read_obj::<u8>(private).unwrap(), 0x5a);
    memory.write_obj(role, private).unwrap();
    let mut stream = UnixStream::connect(control).unwrap();
    configure(&stream);
    stream.write_all(&[role]).unwrap();

    let mut message = [0];
    while stream.read(&mut message).unwrap() == 1 {
        let offset = match message[0] {
            b'1' => 3 * BLOCK,
            b'2' => 3 * BLOCK + 4096,
            b'x' => break,
            b'!' => {
                // Only this fixture child exits. The kernel closes both
                // descriptors without running the mapping destructors.
                assert_eq!(unsafe { libc::raise(libc::SIGKILL) }, 0);
                unreachable!("SIGKILL returned");
            }
            _ => panic!("unknown fixture command"),
        };
        check_raw_pager(lease.as_raw_fd(), None).unwrap();
        assert_eq!(
            memory.read_obj::<u8>(GuestAddress(offset as u64)).unwrap(),
            0x9b
        );
        assert_eq!(memory.read_obj::<u8>(private).unwrap(), role);
        stream.write_all(&message).unwrap();
    }
    drop(memory);
    drop(restore);
    drop(lease);
    stream.write_all(b"x").unwrap();
}

fn mounted(id: u64, path: &Path) -> bool {
    let table = fs::read_to_string("/proc/self/mountinfo").unwrap();
    let id = id.to_string();
    table.lines().any(|line| {
        let mut fields = line.split(' ');
        fields.next() == Some(id.as_str()) && fields.nth(3) == path.to_str()
    })
}

fn run(first: usize, abrupt: bool) {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    let store_path = directory.path().join("store");
    let store = SnapshotStore::new(&store_path).unwrap();
    let pending = store.begin().unwrap();
    let mut bytes = vec![0; RAM_BYTES];
    bytes[BLOCK..2 * BLOCK].fill(0x5a);
    bytes[3 * BLOCK..].fill(0x9b);
    pending.create_ram().unwrap().write_all(&bytes).unwrap();
    let id = pending
        .publish(&source, b"fixture", compatibility())
        .unwrap();
    fs::write(directory.path().join("snapshot"), &id).unwrap();

    let control = directory.path().join("control");
    let listener = UnixListener::bind(&control).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut supervisor = OwnedChild(command("owner", &control).spawn().unwrap());
    let mut owner = None;
    let mut runners = [None, None];
    await_condition("both compact Raw mappings", || {
        let (mut stream, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return false,
            Err(error) => panic!("accept fixture connection: {error}"),
        };
        configure(&stream);
        match byte(&mut stream) {
            role @ (b'a' | b'b') => {
                assert!(runners[usize::from(role - b'a')].replace(stream).is_none());
            }
            b'o' => assert!(owner.replace(BufReader::new(stream)).is_none()),
            _ => panic!("unknown fixture role"),
        }
        owner.is_some() && runners.iter().all(Option::is_some)
    });
    let mut receipt = owner.unwrap();
    let mut line = String::new();
    receipt.read_line(&mut line).unwrap();
    let (mount_id, mountpoint): (u64, PathBuf) = serde_json::from_str(&line).unwrap();
    assert!(mounted(mount_id, &mountpoint));
    let mut runners = runners.map(Option::unwrap);
    supervisor.0.kill().unwrap();
    assert!(!supervisor.0.wait().unwrap().success());

    for stream in &mut runners {
        stream.write_all(b"1").unwrap();
        assert_eq!(byte(stream), b'1');
    }
    if abrupt {
        runners[first].write_all(b"!").unwrap();
        assert_eq!(runners[first].read(&mut [0]).unwrap(), 0);
    } else {
        runners[first].write_all(b"x").unwrap();
        assert_eq!(byte(&mut runners[first]), b'x');
    }
    assert!(mounted(mount_id, &mountpoint));
    runners[1 - first].write_all(b"2").unwrap();
    assert_eq!(byte(&mut runners[1 - first]), b'2');
    runners[1 - first].write_all(b"x").unwrap();
    assert_eq!(byte(&mut runners[1 - first]), b'x');
    await_condition("ordinary retirement of the recorded mount ID", || {
        !mounted(mount_id, &mountpoint)
            && matches!(mountpoint.symlink_metadata(), Err(error) if error.kind() == io::ErrorKind::NotFound)
    });
    store.open(&id, &compatibility()).unwrap();
    assert_eq!(
        fs::read(store_path.join("objects").join(&id).join("ram.bin")).unwrap(),
        bytes
    );
    eprintln!(
        "compact_raw_lease order={} mount_id={mount_id} supervisor_killed=true first_runner_killed={abrupt} peer_read=true seed_unchanged=true ordinary_retirement=true",
        if first == 0 { "AB" } else { "BA" },
    );
}

#[test]
#[ignore = "requires ordinary Linux FUSE mount and a built pvisor executable; no KVM"]
fn compact_raw_fuse_survives_supervisor_loss() {
    match std::env::var(ROLE).ok().as_deref() {
        Some("owner") => owner(Path::new(&std::env::var_os(CONTROL).unwrap())),
        Some(role @ ("a" | "b")) => consumer(
            Path::new(&std::env::var_os(CONTROL).unwrap()),
            role.as_bytes()[0],
        ),
        Some(_) => panic!("invalid fixture role"),
        None => {
            assert!(std::env::var_os(CLI).is_some(), "{CLI} is required");
            run(0, false);
            run(1, false);
        }
    }
}

#[test]
#[ignore = "requires ordinary Linux FUSE mount and a built pvisor executable; no KVM"]
fn compact_raw_fuse_survives_first_consumer_loss() {
    assert!(std::env::var_os(CLI).is_some(), "{CLI} is required");
    run(0, true);
    run(1, true);
}
