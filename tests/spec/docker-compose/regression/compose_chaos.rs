//! Opt-in stateful Compose faults against real VMs, with replayable action order.
use super::*;
use std::{cell::Cell, io::Write, os::unix::process::ExitStatusExt, process::Child};

const PROJECT: &str = "compose-chaos";
const FOREIGN: &str = "compose-chaos-foreign";
const ACTIONS: [&str; 6] = [
    "restart",
    "recreate",
    "admission",
    "workload-exit",
    "concurrent-up",
    "down-up",
];

fn schedule(seed: u64, round: u64) -> Vec<&'static str> {
    let mut actions = ACTIONS.to_vec();
    actions.sort_by_key(|action| {
        *blake3::hash(format!("{seed}:{round}:{action}").as_bytes()).as_bytes()
    });
    actions
}

/// File-backed output avoids pipe backpressure and detached-owner EOF waits.
/// Only this directly owned frontend is killed; the Store owns VM teardown.
struct Client {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
    started: Instant,
    timeout: Duration,
}

impl Client {
    fn wait(mut self) -> Output {
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if self.started.elapsed() > self.timeout {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!(
                    "Compose frontend exceeded {:?}; stderr: {}",
                    self.timeout,
                    fs::read_to_string(&self.stderr).unwrap_or_default()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let output = Output {
            status,
            stdout: fs::read(&self.stdout).unwrap(),
            stderr: fs::read(&self.stderr).unwrap(),
        };
        eprintln!("result: {output:?}");
        output
    }

    fn kill(mut self) {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "frontend exited before the synchronized fault"
        );
        self.child.kill().unwrap();
        assert_eq!(self.child.wait().unwrap().signal(), Some(libc::SIGKILL));
        eprintln!("injected SIGKILL into owned frontend {}", self.child.id());
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Campaign {
    native: Native,
    store: pvisor::docker::Store,
    file: PathBuf,
    document: serde_json::Value,
    sequence: Cell<u64>,
}

impl Campaign {
    fn new() -> Self {
        let native = Native::new();
        let root = native.home.parent().unwrap();
        let control = root.join("control");
        fs::create_dir(&control).unwrap();
        fs::write(control.join("ready"), b"ready").unwrap();
        let file = root.join("chaos.json");
        let document = serde_json::json!({
            "services": {
                "app": {
                    "image": "fixture:latest", "cpus": 1, "mem_limit": "128m",
                    "command": ["sh", "-c",
                        "while test ! -e /control/crash; do sleep 0.1; done; exit 23"],
                    "environment": {"VERSION": "initial"},
                    "volumes": [format!("{}:/control:ro", control.display()), "data:/data"],
                    "depends_on": {"peer": {"condition": "service_healthy"}}
                },
                "peer": {
                    "image": "fixture:latest", "cpus": 1, "mem_limit": "128m",
                    "command": ["sh", "-c", "sleep 3600"],
                    "volumes": [format!("{}:/control:ro", control.display())],
                    "healthcheck": {
                        "test": ["CMD", "test", "-f", "/control/ready"],
                        "interval": "100ms", "timeout": "1s", "retries": 1000
                    }
                }
            },
            "volumes": {"data": {}}
        });
        fs::write(&file, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
        let store = pvisor::docker::Store::open(native.home.clone()).unwrap();
        Self {
            native,
            store,
            file,
            document,
            sequence: Cell::new(0),
        }
    }

    fn root(&self) -> &Path {
        self.native.home.parent().unwrap()
    }

    fn write(&self, document: &serde_json::Value) {
        fs::write(&self.file, serde_json::to_vec_pretty(document).unwrap()).unwrap();
    }

    fn spawn(&self, project: &str, args: &[&str]) -> Client {
        let sequence = self.sequence.get();
        self.sequence.set(sequence + 1);
        let stdout = self.root().join(format!("{sequence:04}.stdout"));
        let stderr = self.root().join(format!("{sequence:04}.stderr"));
        let argv = [
            vec!["compose", "-f", self.file.to_str().unwrap(), "-p", project],
            args.to_vec(),
        ]
        .concat();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.root().join("commands.jsonl"))
                .unwrap(),
            "{}",
            serde_json::json!({"sequence": sequence, "argv": argv})
        )
        .unwrap();
        eprintln!("command {sequence}: {argv:?}");
        let child = Command::new(&self.native.binary)
            .current_dir(self.root())
            .env_remove("COMPOSE_PROJECT_NAME")
            .env_remove("PVISOR_IMAGE_STORE")
            .env("PVISOR_DOCKER_HOME", &self.native.home)
            .env("PVISOR_RUN_HOME", self.root().join("sessions"))
            .args(argv)
            .stdin(Stdio::null())
            .stdout(fs::File::create(&stdout).unwrap())
            .stderr(fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        Client {
            child,
            stdout,
            stderr,
            started: Instant::now(),
            timeout: Duration::from_secs(90),
        }
    }

    fn call(&self, project: &str, args: &[&str], code: i32) -> Output {
        let output = self.spawn(project, args).wait();
        assert_eq!(output.status.code(), Some(code), "{args:?}: {output:?}");
        output
    }

    fn up(&self) {
        self.call(
            PROJECT,
            &["up", "--wait", "--wait-timeout", "20", "-t", "0"],
            0,
        );
    }

    fn exec(&self, script: &str) -> Vec<u8> {
        self.call(
            PROJECT,
            &[
                "exec",
                "-T",
                "--interactive=false",
                "app",
                "sh",
                "-c",
                script,
            ],
            0,
        )
        .stdout
    }

    fn instance(&self, project: &str, service: &str) -> pvisor::docker::Instance {
        self.store
            .resolve(&format!("{project}-{service}-1"))
            .unwrap()
    }

    fn wait_for(&self, description: &str, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !ready() {
            assert!(Instant::now() < deadline, "timed out: {description}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn down(&self, project: &str) {
        self.call(project, &["down", "-v", "-t", "0", "--remove-orphans"], 0);
        assert!(self.store.instances(true).unwrap().iter().all(|instance| {
            instance
                .compose
                .as_ref()
                .is_none_or(|member| member.project != project)
        }));
        assert!(
            self.store
                .networks()
                .unwrap()
                .iter()
                .all(|network| network.name != format!("{project}_default"))
        );
        assert!(
            !self
                .native
                .home
                .join(format!("volumes/{project}_data"))
                .exists()
        );
    }
}

impl Drop for Campaign {
    fn drop(&mut self) {
        // Explicit successful cleanup is asserted by each test. On failure,
        // preserve command logs and state at their original pinned paths.
        if std::thread::panicking()
            || std::env::var("PVISOR_COMPOSE_CHAOS_KEEP").as_deref() == Ok("1")
        {
            self.native._temporary.disable_cleanup(true);
            eprintln!("Compose chaos evidence retained: {}", self.root().display());
        }
        for instance in self.store.instances(true).unwrap_or_default() {
            if instance
                .compose
                .as_ref()
                .is_some_and(|member| [PROJECT, FOREIGN].contains(&member.project.as_str()))
                && let Err(error) = self.store.stop(&instance.id, Duration::ZERO, None)
            {
                self.native._temporary.disable_cleanup(true);
                eprintln!("cleanup failed for {}: {error:#}", instance.id);
                eprintln!("Compose chaos evidence retained: {}", self.root().display());
            }
        }
    }
}

#[test]
fn compose_chaos_schedule_is_repeatable_and_never_omits_a_fault() {
    for seed in [0, 1, 1729, u64::MAX] {
        for round in 0..10 {
            let actions = schedule(seed, round);
            assert_eq!(actions, schedule(seed, round));
            let mut sorted = actions;
            sorted.sort();
            let mut expected = ACTIONS.to_vec();
            expected.sort();
            assert_eq!(sorted, expected);
        }
    }
    assert_ne!(schedule(0, 0), schedule(1, 0));
}

#[test]
fn compose_chaos_frontend_deadline_kills_and_reaps_the_owned_child() {
    let directory = tempfile::tempdir().unwrap();
    let stdout = directory.path().join("stdout");
    let stderr = directory.path().join("stderr");
    let child = Command::new("sleep")
        .arg("30")
        .stdout(fs::File::create(&stdout).unwrap())
        .stderr(fs::File::create(&stderr).unwrap())
        .spawn()
        .unwrap();
    let pid = child.id();
    let client = Client {
        child,
        stdout,
        stderr,
        started: Instant::now(),
        timeout: Duration::from_millis(40),
    };
    assert!(std::panic::catch_unwind(|| client.wait()).is_err());
    let mut status = 0;
    // SAFETY: status points to a writable integer; waitpid targets only our child.
    assert_eq!(
        unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
#[ignore = "requires native virtualization, firmware and a native image with sh/cat/sleep/test"]
fn compose_native_seeded_fault_sequence_preserves_data_and_project_isolation() {
    let seed: u64 = std::env::var("PVISOR_COMPOSE_CHAOS_SEED")
        .unwrap_or_else(|_| "1729".into())
        .parse()
        .unwrap();
    let rounds: u64 = std::env::var("PVISOR_COMPOSE_CHAOS_ROUNDS")
        .unwrap_or_else(|_| "1".into())
        .parse()
        .unwrap();
    assert!((1..=1000).contains(&rounds), "rounds must be 1..=1000");
    let mut case = Campaign::new();
    eprintln!(
        "seed={seed}, rounds={rounds}, root={}",
        case.root().display()
    );
    fs::write(
        case.root().join("campaign.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "seed": seed, "rounds": rounds,
            "binary_blake3": blake3::hash(&fs::read(&case.native.binary).unwrap()).to_hex().to_string(),
            "image": case.store.images().unwrap()[0].digest,
            "schedule": (0..rounds).map(|round| schedule(seed, round)).collect::<Vec<_>>()
        }))
        .unwrap(),
    )
    .unwrap();
    case.up();
    // A live same-store foreign project is an independent teardown oracle.
    case.call(FOREIGN, &["up", "--wait", "--no-deps", "peer"], 0);
    let foreign = case.instance(FOREIGN, "peer");
    let token = format!("volume-{seed}");
    case.exec(&format!("printf {token} > /data/kept"));

    for round in 0..rounds {
        for action in schedule(seed, round) {
            eprintln!("seed={seed} round={round} action={action}");
            let app = case.instance(PROJECT, "app");
            let peer = case.instance(PROJECT, "peer");
            case.exec("printf upper > /upper-kept");
            match action {
                "restart" => {
                    case.call(PROJECT, &["stop", "-t", "0", "app"], 0);
                    case.call(PROJECT, &["start", "app"], 0);
                    assert_eq!(case.instance(PROJECT, "app").id, app.id);
                    assert_ne!(case.instance(PROJECT, "app").generation, app.generation);
                    assert_eq!(case.exec("cat /upper-kept"), b"upper");
                }
                "recreate" => {
                    case.document["services"]["app"]["environment"]["VERSION"] =
                        format!("round-{round}").into();
                    case.write(&case.document);
                    case.call(PROJECT, &["up", "-d", "--no-recreate", "app"], 0);
                    assert_eq!(case.instance(PROJECT, "app").generation, app.generation);
                    assert_eq!(
                        case.instance(PROJECT, "app").env["VERSION"],
                        app.env["VERSION"]
                    );
                    assert_eq!(case.exec("cat /upper-kept"), b"upper");
                    case.up();
                    assert_ne!(case.instance(PROJECT, "app").id, app.id);
                    assert_eq!(
                        case.exec("test ! -e /upper-kept && printf '%s' \"$VERSION\""),
                        format!("round-{round}").as_bytes()
                    );
                }
                "admission" => {
                    let mut invalid = case.document.clone();
                    invalid["services"]["app"]["volumes"]
                        .as_array_mut()
                        .unwrap()
                        .push(
                            format!("{}:/missing:ro", case.root().join("absent").display()).into(),
                        );
                    case.write(&invalid);
                    let failed = case.call(PROJECT, &["up", "-d"], 1);
                    assert!(String::from_utf8_lossy(&failed.stderr).contains("bind source"));
                    assert_eq!(case.instance(PROJECT, "app").generation, app.generation);
                    assert_eq!(case.exec("cat /upper-kept"), b"upper");
                    case.write(&case.document);
                    case.up();
                    assert_eq!(case.instance(PROJECT, "app").id, app.id);
                }
                "workload-exit" => {
                    fs::write(case.root().join("control/crash"), b"exit23").unwrap();
                    case.wait_for("workload exit 23", || {
                        let current = case.instance(PROJECT, "app");
                        current.state == "exited" && current.exit_code == Some(23)
                    });
                    fs::remove_file(case.root().join("control/crash")).unwrap();
                    case.up();
                    assert_eq!(case.instance(PROJECT, "app").id, app.id);
                    assert_ne!(case.instance(PROJECT, "app").generation, app.generation);
                    assert_eq!(case.exec("cat /upper-kept"), b"upper");
                }
                "concurrent-up" => {
                    let first = case.spawn(PROJECT, &["up", "--wait"]);
                    let second = case.spawn(PROJECT, &["up", "--wait"]);
                    assert!(first.wait().status.success());
                    assert!(second.wait().status.success());
                    assert_eq!(case.instance(PROJECT, "app").generation, app.generation);
                    assert_eq!(case.exec("cat /upper-kept"), b"upper");
                }
                "down-up" => {
                    case.call(PROJECT, &["down", "-t", "0"], 0);
                    case.up();
                    assert_ne!(case.instance(PROJECT, "app").id, app.id);
                    case.exec("test ! -e /upper-kept");
                }
                _ => unreachable!(),
            }
            case.up();
            assert_eq!(case.exec("cat /data/kept"), token.as_bytes());
            let current_peer = case.instance(PROJECT, "peer");
            if action != "down-up" {
                assert_eq!(current_peer.id, peer.id);
                assert_eq!(current_peer.generation, peer.generation);
            }
            let current_foreign = case.instance(FOREIGN, "peer");
            assert_eq!(current_foreign.id, foreign.id);
            assert_eq!(current_foreign.generation, foreign.generation);
            assert_eq!(current_foreign.state, "running");
            let records = case.store.instances(true).unwrap();
            assert_eq!(
                records.len(),
                3,
                "no duplicate or pending members after recovery"
            );
            assert!(records.iter().all(|instance| {
                instance.compose.as_ref().unwrap().replaces.is_none() && instance.state == "running"
            }));
        }
    }
    case.down(PROJECT);
    assert_eq!(
        case.instance(FOREIGN, "peer").generation,
        foreign.generation
    );
    case.down(FOREIGN);
    assert!(case.store.instances(true).unwrap().is_empty());
}

#[test]
#[ignore = "requires native virtualization, firmware and a native image with sh/cat/sleep/test"]
fn compose_native_killed_wait_frontend_releases_lease_and_recovers() {
    let case = Campaign::new();
    fs::remove_file(case.root().join("control/ready")).unwrap();
    let client = case.spawn(PROJECT, &["up", "--wait", "--wait-timeout", "60"]);
    case.wait_for(
        "dependency running and dependent admitted but unstarted",
        || {
            let records = case.store.instances(true).unwrap();
            records.len() == 2
                && records.iter().any(|instance| {
                    instance.compose.as_ref().unwrap().service == "peer"
                        && instance.state == "running"
                        && case
                            .store
                            .health(&instance.id)
                            .unwrap()
                            .is_some_and(|health| health.exit_code == Some(1))
                })
                && records.iter().any(|instance| {
                    instance.compose.as_ref().unwrap().service == "app"
                        && instance.state == "created"
                        && instance.started_ms == 0
                })
        },
    );
    let peer = case.instance(PROJECT, "peer");
    let app = case.instance(PROJECT, "app");
    client.kill();
    // Owner health supervision must outlive the frontend. No test edits to
    // container records, stale locks or health observations are allowed.
    fs::write(case.root().join("control/ready"), b"ready").unwrap();
    case.wait_for("owner observes recovery without a frontend", || {
        case.store
            .health(&peer.id)
            .unwrap()
            .is_some_and(|health| health.state == pvisor::docker::health::State::Healthy)
    });
    case.up();
    assert_eq!(case.instance(PROJECT, "peer").generation, peer.generation);
    assert_eq!(case.instance(PROJECT, "app").id, app.id);
    assert_ne!(case.instance(PROJECT, "app").generation, app.generation);
    assert_eq!(
        case.call(PROJECT, &["ps", "-aq"], 0)
            .stdout
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .count(),
        2
    );
    case.down(PROJECT);
    // Both interrupted-up and ordinary-down leases must have been released.
    case.down(PROJECT);
}
