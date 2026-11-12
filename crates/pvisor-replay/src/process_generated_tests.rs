//! Bounded native full-duplex and healthy-successor composition. No shell,
//! interpreter, descendants, external services or production test hook.
use super::{ProcessSpec, process_group_exists, run_process};
use crate::error::ReplayErrorKind;
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

const NAME: &str =
    "process::generated_tests::full_duplex_boundaries_and_healthy_successors_preserve_exact_bytes";
const CHILD: &str = "PVISOR_REPLAY_GENERATED_CHILD";
const INPUT_LENGTHS: [usize; 20] = [
    0, 1, 4095, 4096, 4097, 65535, 65536, 65537, 131071, 131072, 131073, 262143, 262144, 262145,
    524287, 524288, 524289, 1048575, 1048576, 1048577,
];
const TAIL_LENGTHS: [usize; 11] = [
    0, 1, 4095, 4096, 4097, 16383, 16384, 16385, 65536, 65537, 65538,
];

fn child(mode: &str) -> ! {
    // An independent finite bound if process supervision regresses. All child
    // paths are supplied from the exclusively created parent fixture.
    unsafe { libc::alarm(8) };
    let directory = PathBuf::from(std::env::var_os("PVISOR_REPLAY_GENERATED_DIR").unwrap());
    fs::write(directory.join("pid"), std::process::id().to_string()).unwrap();
    if mode == "close-input" {
        unsafe {
            libc::close(0);
            libc::_exit(23);
        }
    }
    assert_eq!(mode, "duplex");
    let out = fs::read(directory.join("prelude.out")).unwrap();
    let err = fs::read(directory.join("prelude.err")).unwrap();
    // Both bursts precede input consumption and exceed ordinary pipe capacity.
    // Blocking delivery without simultaneous output draining would deadlock.
    io::stdout().write_all(&out).unwrap();
    io::stdout().flush().unwrap();
    io::stderr().write_all(&err).unwrap();
    io::stderr().flush().unwrap();
    // An actual half-close: stdout and stdin must still complete independently.
    assert_eq!(unsafe { libc::close(2) }, 0);
    let mut received = Vec::new();
    io::stdin().read_to_end(&mut received).unwrap();
    fs::write(directory.join("received"), &received).unwrap();
    io::stdout().write_all(&received).unwrap();
    io::stdout().flush().unwrap();
    let code: i32 = std::env::var("PVISOR_REPLAY_GENERATED_EXIT")
        .unwrap()
        .parse()
        .unwrap();
    // No libtest footer or buffered bytes after the exact expected final body.
    unsafe { libc::_exit(code) };
}

fn bytes(seed: u64, length: usize, salt: u64) -> Vec<u8> {
    let mut state = seed ^ salt;
    (0..length)
        .map(|_| {
            state = state.wrapping_add(0x9e3779b97f4a7c15);
            let mut value = state;
            value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
            (value ^ (value >> 31)) as u8
        })
        .collect()
}

fn spec(directory: &Path, mode: &str) -> ProcessSpec {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", NAME, "--nocapture"])
        .current_dir(directory)
        .env("PATH", "")
        .env("HOME", directory.join("home"))
        .env("XDG_CONFIG_HOME", directory.join("home/config"))
        .env("XDG_CACHE_HOME", directory.join("home/cache"))
        .env("XDG_STATE_HOME", directory.join("home/state"))
        .env(CHILD, mode)
        .env("PVISOR_REPLAY_GENERATED_DIR", directory)
        .env("PVISOR_REPLAY_GENERATED_EXIT", "0");
    ProcessSpec {
        command,
        stdin: None,
        idle_timeout: None,
        step_finish_limit: None,
        stdout_redirect: None,
        timeout: Duration::from_secs(5),
        termination_grace: Duration::from_millis(100),
        pipe_grace: Duration::from_millis(100),
        retained_bytes: 4096,
        log_path: directory.join("process.log"),
        log_bytes_limit: None,
    }
}

fn assert_reaped(directory: &Path) {
    let pid: i32 = fs::read_to_string(directory.join("pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(pid > 0);
    // Read-only ownership check: never signal a PID recovered from a stale file.
    let result = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
    let error = io::Error::last_os_error();
    assert_eq!(result, -1, "supervisor did not reap its child {pid}");
    assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
    assert!(
        !process_group_exists(pid).unwrap(),
        "process group {pid} remains"
    );
}

fn tail(bytes: &[u8], retained: usize) -> &[u8] {
    &bytes[bytes.len().saturating_sub(retained)..]
}

#[derive(Default, Debug)]
struct Counts {
    duplex_children: usize,
    redirected_children: usize,
    nonzero_exits: usize,
    expected_stdin_errors: usize,
    healthy_successors: usize,
    input_bytes: usize,
}

fn duplex(
    seed: u64,
    root: &Path,
    role: &str,
    redirected: bool,
    successor: bool,
    counts: &mut Counts,
) {
    let directory = root.join(role);
    fs::create_dir(&directory).unwrap();
    fs::create_dir(directory.join("home")).unwrap();
    let input_index = if successor {
        seed.wrapping_add(7)
    } else {
        seed
    };
    let input = bytes(
        seed,
        INPUT_LENGTHS[input_index as usize % INPUT_LENGTHS.len()],
        13,
    );
    let out = bytes(seed, 131073, 17);
    let err = bytes(seed, 65537, 23);
    fs::write(directory.join("prelude.out"), &out).unwrap();
    fs::write(directory.join("prelude.err"), &err).unwrap();
    fs::write(directory.join("process.log"), b"owned stale log").unwrap();
    let retained_index = if successor {
        seed.wrapping_add(5)
    } else {
        seed
    };
    let retained = TAIL_LENGTHS[retained_index as usize % TAIL_LENGTHS.len()];
    let exit = if successor {
        0
    } else {
        [0, 7, 23][seed as usize % 3]
    };
    let mut request = spec(&directory, "duplex");
    request
        .command
        .env("PVISOR_REPLAY_GENERATED_EXIT", exit.to_string());
    request.stdin = Some(input.clone());
    request.retained_bytes = retained;
    let redirected_path = directory.join("stdout");
    request.stdout_redirect = redirected.then(|| redirected_path.clone());
    let started = Instant::now();
    let result =
        run_process(request).unwrap_or_else(|error| panic!("seed={seed} role={role}: {error}"));
    assert!(
        started.elapsed() < Duration::from_secs(7),
        "seed={seed} role={role}"
    );
    assert_reaped(&directory);
    assert_eq!(result.status.code(), Some(exit), "seed={seed} role={role}");
    assert!(!result.timed_out && !result.step_limited && !result.background_cleanup);
    // The received bytes come only from supervised stdin, never from the oracle
    // fixture files. This also proves successful delivery reached actual EOF.
    assert_eq!(fs::read(directory.join("received")).unwrap(), input);
    let mut expected_stdout = out;
    expected_stdout.extend_from_slice(&input);
    assert_eq!(result.stdout_tail, tail(&expected_stdout, retained));
    assert_eq!(result.stderr_tail, tail(&err, retained));
    assert!(result.stdout_truncated); // Body exceeds the largest retained window.
    assert_eq!(result.stderr_truncated, err.len() > retained);
    let log = fs::read(directory.join("process.log")).unwrap();
    assert_eq!(
        fs::metadata(directory.join("process.log")).unwrap().mode() & 0o777,
        0o600
    );
    if redirected {
        assert_eq!(log, err);
        let stdout = fs::read(&redirected_path).unwrap();
        let scaffold = stdout
            .strip_suffix(expected_stdout.as_slice())
            .expect("redirect did not preserve the complete child stdout body");
        // Libtest emits its own prefix before entering the native fixture. It
        // is not counted as fixture payload or used to compute expected tails.
        assert!(scaffold.len() < 4096);
        counts.redirected_children += 1;
    } else {
        // Cross-stream ordering is deliberately unspecified. Verify total bytes
        // and both exact channel tails, not an invented concatenation order.
        let payload_len = expected_stdout.len() + err.len();
        assert!((payload_len..payload_len + 4096).contains(&log.len()));
        assert!(!redirected_path.exists());
    }
    counts.duplex_children += 1;
    counts.nonzero_exits += usize::from(exit != 0);
    counts.healthy_successors += usize::from(successor);
    counts.input_bytes += input.len();
}

fn run_seed(seed: u64, counts: &mut Counts) {
    let root = tempfile::Builder::new()
        .prefix(&format!("replay5-{seed}-"))
        .tempdir()
        .unwrap()
        .keep();
    // Keep failure fixtures, with the exact seed and per-child PID/log/input
    // evidence. Cleanup is only on confirmed success, never from a Drop sweep.
    fs::write(root.join("seed"), seed.to_string()).unwrap();
    duplex(seed, &root, "pipe", false, false, counts);
    duplex(seed, &root, "redirect", true, false, counts);
    let failure = root.join("closed");
    fs::create_dir(&failure).unwrap();
    fs::create_dir(failure.join("home")).unwrap();
    let mut request = spec(&failure, "close-input");
    request.stdin = Some(bytes(seed, 1048577, 29));
    let error = run_process(request)
        .map(|_| ())
        .expect_err("early stdin closure accepted");
    assert_eq!(error.kind, ReplayErrorKind::Executor);
    assert!(
        error.message.contains("write supervised process stdin"),
        "{error}"
    );
    assert_reaped(&failure);
    counts.expected_stdin_errors += 1;
    duplex(
        seed,
        &root,
        "successor",
        seed.is_multiple_of(2),
        true,
        counts,
    );
    fs::remove_dir_all(&root).unwrap();
}

fn parameter(name: &str, default: u64) -> u64 {
    std::env::var(name).map_or(default, |value| value.parse().expect(name))
}

#[test]
fn full_duplex_boundaries_and_healthy_successors_preserve_exact_bytes() {
    if let Ok(mode) = std::env::var(CHILD) {
        child(&mode);
    }
    let first = parameter("PVISOR_CHAOS_FIRST_SEED", 202611120001);
    let count = parameter("PVISOR_CHAOS_SEED_COUNT", 20);
    assert!((1..=4096).contains(&count), "seed count must be 1..4096");
    first.checked_add(count).expect("seed interval overflow");
    let started = Instant::now();
    let mut counts = Counts::default();
    for seed in first..first + count {
        run_seed(seed, &mut counts);
    }
    eprintln!(
        "Replay duplex first={first} seeds={count} elapsed={:?} counts={counts:?}",
        started.elapsed()
    );
}
