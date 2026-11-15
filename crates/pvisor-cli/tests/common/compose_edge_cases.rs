//! Opt-in real VM cases for recovery, authority and concurrent convergence.
use super::*;
use std::os::unix::fs::PermissionsExt;

struct Case {
    native: Native,
    store: pvisor::docker::Store,
    file: PathBuf,
}

impl Case {
    fn new() -> Self {
        let native = Native::new();
        let store = pvisor::docker::Store::open(native.home.clone()).unwrap();
        let file = native.home.parent().unwrap().join("edge-compose.json");
        Self {
            native,
            store,
            file,
        }
    }

    fn root(&self) -> &Path {
        self.native.home.parent().unwrap()
    }

    fn write(&self, mut document: serde_json::Value) {
        document["name"] = "compose-edge".into();
        fs::write(&self.file, serde_json::to_vec(&document).unwrap()).unwrap();
    }

    fn command(&self, args: &[&str]) -> Vec<String> {
        let mut command = vec![
            "compose".into(),
            "-f".into(),
            self.file.to_str().unwrap().into(),
        ];
        command.extend(args.iter().map(|arg| (*arg).into()));
        command
    }

    fn call(&self, args: &[&str], expected: i32) -> Output {
        let command = self.command(args);
        let command: Vec<_> = command.iter().map(String::as_str).collect();
        self.native.call(&command, None, expected)
    }

    fn exec(&self, service: &str, script: &str, expected: i32) -> Output {
        self.call(
            &[
                "exec",
                "-T",
                "--interactive=false",
                service,
                "sh",
                "-c",
                script,
            ],
            expected,
        )
    }

    fn instance(&self, service: &str) -> pvisor::docker::Instance {
        self.store
            .resolve(&format!("compose-edge-{service}-1"))
            .unwrap()
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        for instance in self.store.instances(true).unwrap_or_default() {
            if instance
                .compose
                .as_ref()
                .is_some_and(|member| member.project == "compose-edge")
            {
                if std::thread::panicking() {
                    eprintln!(
                        "FAILED EDGE CASE: {} {} {:?}",
                        instance.name, instance.state, instance.exit_code
                    );
                    let _ = self.store.logs(
                        &instance.id,
                        false,
                        &mut std::io::stderr(),
                        &mut std::io::stderr(),
                    );
                }
                let _ = self.store.stop(&instance.id, Duration::ZERO, None);
                let _ = self.store.remove(&instance.id, false);
            }
        }
        let _ = self.store.remove_network("compose-edge_default", true);
    }
}

fn write_mode(path: &Path, bytes: &[u8], mode: u32) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
#[ignore = "requires native virtualization, firmware and registry access for python:3.12-alpine"]
fn compose_native_nonroot_unix_socket_and_boot_hostname_survive_restart() {
    let case = Case::new();
    let python =
        std::env::var("PVISOR_COMPOSE_TEST_PYTHON").unwrap_or_else(|_| "python:3.12-alpine".into());
    case.native.call(&["pull", &python], None, 0);
    // The released Python image supplies its own hosts file, which must remain
    // untouched. Exercise the separate generated-hosts contract deliberately.
    let context = case.root().join("socket-image");
    fs::create_dir(&context).unwrap();
    fs::write(
        context.join("Dockerfile"),
        format!("FROM {python}\nRUN rm -f /etc/hosts\n"),
    )
    .unwrap();
    case.native.call(
        &[
            "build",
            "-t",
            "socket-python:latest",
            context.to_str().unwrap(),
        ],
        None,
        0,
    );
    let server = r#"
import os, socket
path = "/tmp/compose.sock"
try: os.unlink(path)
except FileNotFoundError: pass
os.umask(0o077)
with socket.socket(socket.AF_UNIX) as server:
    server.bind(path)
    server.listen()
    while True:
        peer, _ = server.accept()
        with peer:
            assert peer.recv(4) == b"ping"
            peer.sendall(b"pong")
"#;
    let client = "import socket; s=socket.socket(socket.AF_UNIX); s.connect('/tmp/compose.sock'); s.sendall(b'ping'); assert s.recv(4)==b'pong'";
    case.write(serde_json::json!({"services":{"app":{
        "image":"socket-python:latest","cpus":1,"mem_limit":"128m","user":"65534:65534",
        "command":["python","-u","-c",server],
        "healthcheck":{"test":["CMD","python","-c",client],"interval":"100ms",
            "timeout":"1s","retries":5,"start_period":"2s","start_interval":"100ms"}
    }}}));
    case.call(&["up", "--wait", "--wait-timeout", "15"], 0);
    let initial = case.instance("app");
    let inspect = r#"
import os, socket, stat
st = os.stat("/tmp/compose.sock")
assert stat.S_ISSOCK(st.st_mode)
# Linux bind starts with 0777, so umask 077 yields 0700 (not regular-file 0600).
assert (st.st_uid, st.st_gid, stat.S_IMODE(st.st_mode)) == (65534, 65534, 0o700), st
host = socket.gethostname()
assert host.startswith("pvisor-") and host != "localhost", host
try:
    assert socket.gethostbyname(host) == "127.0.0.1"
except Exception:
    import sys
    print("hostname:", host, "hosts stat:", os.stat("/etc/hosts"), file=sys.stderr)
    print(open("/etc/hosts").read(), file=sys.stderr)
    raise
assert socket.getfqdn("0.0.0.0") != "(none)"
print(host)
"#;
    let observed = case.call(
        &[
            "exec",
            "-T",
            "--interactive=false",
            "app",
            "python",
            "-c",
            inspect,
        ],
        0,
    );
    let denied = case.call(
        &[
            "exec",
            "-T",
            "--interactive=false",
            "-u",
            "65533:65533",
            "app",
            "python",
            "-c",
            client,
        ],
        1,
    );
    assert!(
        String::from_utf8_lossy(&denied.stderr).contains("PermissionError"),
        "{denied:?}"
    );
    case.call(&["stop", "-t", "0", "app"], 0);
    case.call(&["start", "app"], 0);
    case.call(&["up", "--wait", "--wait-timeout", "15"], 0);
    let restarted = case.instance("app");
    assert_eq!(restarted.id, initial.id);
    assert_ne!(restarted.generation, initial.generation);
    let after = case.call(
        &[
            "exec",
            "-T",
            "--interactive=false",
            "app",
            "python",
            "-c",
            inspect,
        ],
        0,
    );
    assert_ne!(
        after.stdout, observed.stdout,
        "cold boots must have distinct fallback hostnames"
    );
}

#[test]
#[ignore = "requires native virtualization, firmware and a native image with sh/cat/sleep/id"]
fn compose_native_nonroot_atomic_file_replacement_and_missing_source_recovery() {
    let case = Case::new();
    let input = case.root().join("input");
    let output = case.root().join("output");
    let retired = case.root().join("retired-output");
    write_mode(&input, b"original", 0o444);
    write_mode(&output, b"original-output", 0o666);
    case.write(serde_json::json!({"services":{"app":{
        "image":"fixture:latest","cpus":1,"mem_limit":"128m","user":"65534:65534",
        "working_dir":"/tmp","environment":{"CASE_ENV":"configured"},
        "command":["sh","-c","sleep 300"],
        "volumes":[format!("{}:/input:ro",input.display()),format!("{}:/output:rw",output.display())],
        "healthcheck":{"test":["CMD-SHELL",
            "test \"$(id -u)\" = 65534 && test \"$$CASE_ENV\" = configured && test \"$$PWD\" = /tmp && test -r /input && test -w /output"],
            "interval":"100ms","timeout":"1s","retries":3,"start_period":"1s","start_interval":"100ms"}
    }}}));
    case.call(&["up", "--wait", "--wait-timeout", "15"], 0);
    let initial = case.instance("app");
    let observed = case.exec(
        "app",
        "printf '%s:%s:%s:' \"$(id -u)\" \"$PWD\" \"$CASE_ENV\"; cat /input",
        0,
    );
    assert_eq!(observed.stdout, b"65534:/tmp:configured:original");
    let denied = case.exec("app", "echo denied > /input", 1);
    let denial = String::from_utf8_lossy(&denied.stderr).to_ascii_lowercase();
    assert!(
        denial.contains("read-only file system") || denial.contains("permission denied"),
        "{denied:?}"
    );
    // Atomic pathname replacement must not retarget a running bind. Preserve
    // the old writable inode separately so writeback authority is observable.
    fs::hard_link(&output, &retired).unwrap();
    let replacement = case.root().join("replacement");
    write_mode(&replacement, b"replacement", 0o444);
    fs::rename(&replacement, &input).unwrap();
    write_mode(&replacement, b"replacement-output", 0o666);
    fs::rename(&replacement, &output).unwrap();
    assert_eq!(
        case.exec("app", "cat /input; printf old-inode > /output", 0)
            .stdout,
        b"original"
    );
    assert_eq!(fs::read(&retired).unwrap(), b"old-inode");
    assert_eq!(fs::read(&output).unwrap(), b"replacement-output");
    case.call(&["stop", "-t", "0", "app"], 0);
    case.call(&["start", "app"], 0);
    case.call(&["up", "--wait", "--wait-timeout", "15", "app"], 0);
    let restarted = case.instance("app");
    assert_eq!(restarted.id, initial.id);
    assert_ne!(restarted.generation, initial.generation);
    assert_eq!(
        case.exec("app", "cat /input; printf new-inode > /output", 0)
            .stdout,
        b"replacement"
    );
    assert_eq!(fs::read(&output).unwrap(), b"new-inode");
    assert_eq!(fs::read(&retired).unwrap(), b"old-inode");
    // Deletion still leaves the running inode readable. A new launch must fail
    // rather than silently sharing a different object, and can retry on restore.
    fs::remove_file(&input).unwrap();
    assert_eq!(case.exec("app", "cat /input", 0).stdout, b"replacement");
    case.call(&["stop", "-t", "0", "app"], 0);
    case.call(&["start", "app"], 1);
    assert_eq!(case.instance("app").state, "exited");
    write_mode(&input, b"restored", 0o444);
    case.call(&["start", "app"], 0);
    case.call(&["up", "--wait", "--wait-timeout", "15", "app"], 0);
    assert_eq!(case.instance("app").id, initial.id);
    assert_eq!(case.exec("app", "cat /input", 0).stdout, b"restored");
}

#[test]
#[ignore = "requires native virtualization, firmware and a native image with sh/cat/sleep"]
fn compose_native_failed_completion_job_recovers_without_rerunning_success() {
    let case = Case::new();
    let gate = case.root().join("gate");
    fs::write(&gate, b"blocked").unwrap();
    case.write(serde_json::json!({"services":{
        "job":{"image":"fixture:latest","cpus":1,"mem_limit":"128m",
            "command":["sh","-c","test \"$(cat /gate)\" = ready || exit 17"],
            "volumes":[format!("{}:/gate:ro",gate.display())]},
        "app":{"image":"fixture:latest","cpus":1,"mem_limit":"128m","command":["sh","-c","sleep 300"],
            "depends_on":{"job":{"condition":"service_completed_successfully"}}}
    }}));
    let failed = case.call(&["up", "--wait", "--wait-timeout", "15"], 1);
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("17"),
        "{failed:?}"
    );
    let job = case.instance("job");
    let blocked = case.instance("app");
    assert_eq!(job.exit_code, Some(17));
    assert_eq!(blocked.state, "created");
    assert_eq!(blocked.started_ms, 0);
    fs::write(&gate, b"ready").unwrap();
    case.call(&["up", "--wait", "--wait-timeout", "15"], 0);
    let completed = case.instance("job");
    let app = case.instance("app");
    assert_eq!(completed.id, job.id);
    assert_ne!(completed.generation, job.generation);
    assert_eq!(completed.exit_code, Some(0));
    assert_eq!(app.id, blocked.id);
    assert_eq!(app.state, "running");
    case.call(&["up", "--wait", "--wait-timeout", "15"], 0);
    assert_eq!(case.instance("job").generation, completed.generation);
    assert_eq!(case.instance("app").generation, app.generation);
    case.call(&["stop", "-t", "0", "app"], 0);
    case.call(&["up", "--wait", "--wait-timeout", "15", "app"], 0);
    assert_eq!(case.instance("job").generation, completed.generation);
    assert_eq!(case.instance("app").id, app.id);
    assert_ne!(case.instance("app").generation, app.generation);
}

#[test]
#[ignore = "requires native virtualization, firmware and a native image with sh/cat/sleep"]
fn compose_native_unhealthy_dependency_recovers_and_no_recreate_keeps_effective_health() {
    let case = Case::new();
    let gate = case.root().join("gate");
    fs::write(&gate, b"blocked").unwrap();
    let mut document = serde_json::json!({"services":{
        "db":{"image":"fixture:latest","cpus":1,"mem_limit":"128m","command":["sh","-c","sleep 300"],
            "volumes":[format!("{}:/gate:ro",gate.display())],
            "healthcheck":{"test":["CMD-SHELL","test \"$(cat /gate)\" = ready"],
                "interval":"100ms","timeout":"1s","retries":1}},
        "app":{"image":"fixture:latest","cpus":1,"mem_limit":"128m","command":["sh","-c","sleep 300"],
            "depends_on":{"db":{"condition":"service_healthy"}}}
    }});
    case.write(document.clone());
    let failed = case.call(&["up", "--wait", "--wait-timeout", "15"], 1);
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("unhealthy"),
        "{failed:?}"
    );
    let db = case.instance("db");
    assert_eq!(db.state, "running");
    assert_eq!(case.instance("app").started_ms, 0);
    fs::write(&gate, b"ready").unwrap();
    // Let the owner observe recovery before the next up, which correctly fails
    // fast on an existing unhealthy observation rather than waiting it away.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if case.store.health(&db.id).unwrap().unwrap().state
            == pvisor::docker::health::State::Healthy
        {
            break;
        }
        assert!(Instant::now() < deadline, "owner health did not recover");
        std::thread::sleep(Duration::from_millis(20));
    }
    case.call(&["up", "--wait", "--wait-timeout", "15"], 0);
    let app = case.instance("app");
    assert_eq!(case.instance("db").generation, db.generation);
    document["services"]["db"]["healthcheck"]["test"] = serde_json::json!(["CMD", "false"]);
    document["services"]["app"]["environment"] = serde_json::json!({"CHANGED":"desired-only"});
    case.write(document);
    case.call(
        &["up", "--wait", "--wait-timeout", "15", "--no-recreate"],
        0,
    );
    assert_eq!(case.instance("db").generation, db.generation);
    assert_eq!(case.instance("app").generation, app.generation);
    assert_eq!(
        case.store.health(&db.id).unwrap().unwrap().state,
        pvisor::docker::health::State::Healthy
    );
    assert!(!case.instance("app").env.contains_key("CHANGED"));
}

#[test]
#[ignore = "requires native virtualization, firmware and a native image with sh/cat/sleep"]
fn compose_native_concurrent_up_and_failed_replacement_preserve_old_members() {
    let case = Case::new();
    let source = case.root().join("source");
    fs::write(&source, b"old-config").unwrap();
    let mut document = serde_json::json!({"services":{
        "a-peer":{"image":"fixture:latest","cpus":1,"mem_limit":"128m","command":["sh","-c","sleep 300"]},
        "z-app":{"image":"fixture:latest","cpus":1,"mem_limit":"128m","command":["sh","-c","sleep 300"],
            "volumes":[format!("{}:/config:ro",source.display())]}
    }});
    case.write(document.clone());
    let mut clients = Vec::new();
    for _ in 0..2 {
        clients.push(
            Command::new(&case.native.binary)
                .env("PVISOR_DOCKER_HOME", &case.native.home)
                .args(case.command(&["up", "--wait", "--wait-timeout", "15"]))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    for client in clients {
        let result = client.wait_with_output().unwrap();
        assert!(result.status.success(), "{result:?}");
    }
    let peer = case.instance("a-peer");
    let app = case.instance("z-app");
    assert_eq!(case.store.instances(true).unwrap().len(), 2);
    case.call(&["up", "--wait", "--wait-timeout", "15"], 0);
    assert_eq!(case.instance("a-peer").generation, peer.generation);
    assert_eq!(case.instance("z-app").generation, app.generation);
    case.exec("z-app", "touch /old-upper", 0);
    for service in ["a-peer", "z-app"] {
        document["services"][service]["environment"] = serde_json::json!({"VERSION":"new"});
    }
    case.write(document);
    fs::remove_file(&source).unwrap();
    let failed = case.call(&["up", "-d"], 1);
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("bind source"),
        "{failed:?}"
    );
    for original in [&peer, &app] {
        let current = case.store.resolve(&original.id).unwrap();
        assert_eq!(current.state, "running");
        assert_eq!(current.generation, original.generation);
    }
    let records = case.store.instances(true).unwrap();
    assert_eq!(
        records.len(),
        3,
        "the earlier admitted candidate is retained for recovery"
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record.compose.as_ref().unwrap().replaces.is_some())
            .count(),
        1
    );
    assert_eq!(
        case.exec("z-app", "test -f /old-upper && cat /config", 0)
            .stdout,
        b"old-config"
    );
    fs::write(&source, b"new-config").unwrap();
    case.call(&["up", "--wait", "--wait-timeout", "15"], 0);
    let current = case.store.instances(true).unwrap();
    assert_eq!(current.len(), 2);
    assert!(
        current
            .iter()
            .all(|record| record.compose.as_ref().unwrap().replaces.is_none())
    );
    assert_ne!(case.instance("a-peer").id, peer.id);
    assert_ne!(case.instance("z-app").id, app.id);
    assert_eq!(
        case.exec("z-app", "test ! -e /old-upper && cat /config", 0)
            .stdout,
        b"new-config"
    );
}
