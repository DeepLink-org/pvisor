//! Real Redis + migration + Python HTTP acceptance; no simulated VM backend.
use super::*;

#[test]
#[ignore = "requires native virtualization, firmware and PVISOR_DOCKER_TEST_IMAGE or PVISOR_DOCKER_TEST_ROOTFS"]
fn compose_native_file_bind_restart_and_started_dependencies() {
    let native = Native::new();
    let root = native.home.parent().unwrap();
    let file = root.join("restart-compose.json");
    let readonly = root.join("readonly");
    let writable = root.join("writable");
    fs::write(&readonly, b"initial").unwrap();
    fs::write(&writable, b"initial").unwrap();
    let mut document = serde_json::json!({
        "name":"compose-restart",
        "services":{
            "short-job":{"image":"fixture:latest","cpus":1,"mem_limit":"128m","command":["true"]},
            "long-job":{"image":"fixture:latest","cpus":1,"mem_limit":"128m","command":["true"]},
            "app":{"image":"fixture:latest","cpus":1,"mem_limit":"128m",
                "command":["sh","-c","sleep 300"],"depends_on":["short-job"],
                "volumes":[format!("{}:/config:ro",readonly.display()),format!("{}:/writable:rw",writable.display())]},
            "other":{"image":"fixture:latest","cpus":1,"mem_limit":"128m",
                "command":["sh","-c","sleep 300"],
                "depends_on":{"long-job":{"condition":"service_started"}}}
        }
    });
    fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
    let store = pvisor::docker::Store::open(native.home.clone()).unwrap();
    struct Cleanup(pvisor::docker::Store);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for instance in self.0.instances(true).unwrap_or_default() {
                if instance
                    .compose
                    .as_ref()
                    .is_some_and(|member| member.project == "compose-restart")
                {
                    let _ = self.0.stop(&instance.id, Duration::ZERO, None);
                    let _ = self.0.remove(&instance.id, false);
                }
            }
        }
    }
    let _cleanup = Cleanup(store.clone());
    let compose = |args: &[&str], expected| {
        let mut command = vec!["compose", "-f", file.to_str().unwrap()];
        command.extend_from_slice(args);
        native.call(&command, None, expected)
    };
    compose(&["up", "-d"], 0);
    for name in ["short-job", "long-job"] {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let job = store.resolve(&format!("compose-restart-{name}-1")).unwrap();
            if job.state == "exited" {
                assert_eq!(job.exit_code, Some(0));
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    assert_eq!(
        store.resolve("compose-restart-other-1").unwrap().state,
        "running"
    );
    compose(&["start", "short-job"], 0);
    let initial = store.resolve("compose-restart-app-1").unwrap();
    compose(
        &[
            "exec",
            "-T",
            "--interactive=false",
            "app",
            "touch",
            "/retained",
        ],
        0,
    );
    let mut generation = initial.generation.clone();
    for cycle in 0..2 {
        compose(&["stop", "-t", "0", "app"], 0);
        let message = format!("host-{cycle}");
        fs::write(&readonly, message.as_bytes()).unwrap();
        compose(&["start", "app"], 0);
        let app = store.resolve(&initial.id).unwrap();
        assert_eq!(app.state, "running");
        assert_ne!(app.generation, generation);
        generation = app.generation;
        let output = compose(
            &[
                "exec",
                "-T",
                "--interactive=false",
                "app",
                "sh",
                "-c",
                "test -f /retained && cat /config && printf guest-write > /writable",
            ],
            0,
        );
        assert_eq!(output.stdout, message.as_bytes());
        assert_eq!(fs::read(&writable).unwrap(), b"guest-write");
        let denied = compose(
            &[
                "exec",
                "-T",
                "--interactive=false",
                "app",
                "sh",
                "-c",
                "echo denied > /config",
            ],
            1,
        );
        assert!(
            String::from_utf8_lossy(&denied.stderr)
                .to_ascii_lowercase()
                .contains("read-only file system")
        );
        assert_eq!(fs::read(&readonly).unwrap(), message.as_bytes());
    }
    // A conflicting image mountpoint must still abort bootstrap, and start
    // must not turn the already-observed nonzero exit into CLI success.
    let context = root.join("collision-context");
    fs::create_dir(&context).unwrap();
    fs::write(context.join("collision"), b"do not cover image data").unwrap();
    fs::write(
        context.join("Dockerfile"),
        "FROM fixture:latest\nCOPY collision /run/pvisor-file-0/foreign\n",
    )
    .unwrap();
    native.call(
        &["build", "-t", "collision:latest", context.to_str().unwrap()],
        None,
        0,
    );
    document["services"]["app"]["image"] = "collision:latest".into();
    fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
    compose(&["up", "--wait", "--no-deps", "app"], 1);
    let failure = compose(&["start", "app"], 1);
    assert!(
        String::from_utf8_lossy(&failure.stderr).contains("125"),
        "{failure:?}"
    );
    let failed = store.resolve("compose-restart-app-1").unwrap();
    assert_eq!(failed.state, "exited");
    assert_eq!(failed.exit_code, Some(125));
    let logs = compose(&["logs", "app"], 0);
    assert!(
        String::from_utf8_lossy(&logs.stderr).contains("empty nonsymlink directory"),
        "{logs:?}"
    );
    compose(&["down", "-t", "0"], 0);
}

#[test]
#[ignore = "requires native virtualization, firmware, sockets and registry access"]
fn compose_native_health_dependencies_and_file_binds() {
    use std::io::{Read, Write};
    use std::time::Instant;
    let native = Native::new();
    let root = native.home.parent().unwrap();
    let file = root.join("health-compose.yaml");
    let app_file = root.join("app.py");
    let client_file = root.join("client.py");
    let migration_file = root.join("migrate.py");
    let config_file = root.join("config.json");
    let writable_file = root.join("writable.txt");
    fs::write(&config_file, r#"{"message":"first","healthy":true}"#).unwrap();
    fs::write(&writable_file, b"host").unwrap();
    fs::write(root.join("secret-sibling"), b"must not be shared").unwrap();
    fs::write(
        &client_file,
        r#"
import socket
def redis(*words):
    with socket.create_connection(("db", 6379), timeout=2) as s:
        parts = [str(word).encode() for word in words]
        s.sendall(b"*" + str(len(parts)).encode() + b"\r\n" +
                  b"".join(b"$" + str(len(p)).encode() + b"\r\n" + p + b"\r\n" for p in parts))
        f = s.makefile("rb")
        line = f.readline()
        if line.startswith(b"$"):
            n = int(line[1:])
            return None if n < 0 else f.read(n + 2)[:-2].decode()
        if line.startswith(b"+"): return line[1:-2].decode()
        raise RuntimeError(line)
"#,
    )
    .unwrap();
    fs::write(&migration_file, "from client import redis\nassert redis('SET','schema','v1') == 'OK'\nprint('migration-completed', flush=True)\n").unwrap();
    fs::write(
        &app_file,
        r#"
print("app-importing", flush=True)
import http.server, json
from client import redis
print("app-checking-migration", flush=True)
assert redis("GET", "schema") == "v1"
print("app-writing-bind", flush=True)
open("/app/writable.txt", "w").write("guest-write")
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        with open("/app/config.json") as f: result = json.load(f)
        result["schema"] = redis("GET", "schema")
        body = json.dumps(result).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
print("app-binding-http", flush=True)
http.server.HTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
"#,
    )
    .unwrap();
    let python =
        std::env::var("PVISOR_COMPOSE_TEST_PYTHON").unwrap_or_else(|_| "python:3.12-alpine".into());
    let redis =
        std::env::var("PVISOR_COMPOSE_TEST_REDIS").unwrap_or_else(|_| "redis:7-alpine".into());
    native.call(&["pull", &python], None, 0);
    native.call(&["pull", &redis], None, 0);
    let check = serde_json::json!({
        "test":["CMD","python","-c","import json,urllib.request; assert json.load(urllib.request.urlopen('http://127.0.0.1:8080/', timeout=1))['healthy']"],
        "interval":"200ms","timeout":"1s","retries":3,"start_period":"3s","start_interval":"100ms"
    });
    let mut document = serde_json::json!({
        "name":"compose-health",
        "services":{
            "db":{"image":redis,"cpus":1,"mem_limit":"128m",
                "command":["redis-server","--appendonly","yes","--dir","/data","--save",""],
                "volumes":["data:/data"],
                "healthcheck":{"test":["CMD","redis-cli","ping"],"interval":"200ms",
                    "timeout":"1s","retries":10,"start_period":"3s","start_interval":"100ms"}},
            "migration":{"image":python,"cpus":1,"mem_limit":"128m",
                "command":["python","/app/migrate.py"],
                "volumes":[format!("{}:/app/migrate.py:ro",migration_file.display()),
                    format!("{}:/app/client.py:ro",client_file.display())],
                "depends_on":{"db":{"condition":"service_healthy"}}},
            "app":{"image":python,"cpus":1,"mem_limit":"128m",
                "command":["python","/app/app.py"],
                "ports":["127.0.0.1:0:8080"],
                "volumes":[format!("{}:/app/app.py:ro",app_file.display()),
                    format!("{}:/app/client.py:ro",client_file.display()),
                    format!("{}:/app/config.json:ro",config_file.display()),
                    format!("{}:/app/writable.txt:rw",writable_file.display())],
                "depends_on":{"db":{"condition":"service_healthy"},
                    "migration":{"condition":"service_completed_successfully"}},
                "healthcheck":check}
        },
        "volumes":{"data":{}}
    });
    fs::write(&file, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    let store = pvisor::docker::Store::open(native.home.clone()).unwrap();
    struct Cleanup(pvisor::docker::Store, PathBuf, PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for instance in self.0.instances(true).unwrap_or_default() {
                if instance
                    .compose
                    .as_ref()
                    .is_some_and(|member| member.project.starts_with("compose-health"))
                {
                    if std::thread::panicking() {
                        eprintln!(
                            "FAILED CASE: {} state={} exit={:?} health={:?}",
                            instance.name,
                            instance.state,
                            instance.exit_code,
                            self.0.health(&instance.id)
                        );
                        let _ = self.0.logs(
                            &instance.id,
                            false,
                            &mut std::io::stderr(),
                            &mut std::io::stderr(),
                        );
                        if instance.state == "running"
                            && let Ok(output) = Command::new(&self.1).env("PVISOR_DOCKER_HOME", &self.2)
                                .args(["exec", &instance.id, "sh", "-c", "ps -ef; for p in /proc/[0-9]*/wchan; do echo \"$p: $(cat \"$p\")\"; done"])
                                .output()
                            {
                                eprintln!("GUEST PROCESSES: {}", String::from_utf8_lossy(&output.stdout));
                        }
                    }
                    let _ = self.0.stop(&instance.id, Duration::ZERO, None);
                    let _ = self.0.remove(&instance.id, false);
                }
            }
        }
    }
    let _cleanup = Cleanup(store.clone(), native.binary.clone(), native.home.clone());
    let compose = |args: &[&str], expected| {
        let mut command = vec!["compose", "-f", file.to_str().unwrap()];
        command.extend_from_slice(args);
        native.call(&command, None, expected)
    };
    let response = |instance: &pvisor::docker::Instance| -> serde_json::Value {
        let mut connection =
            std::net::TcpStream::connect(("127.0.0.1", instance.config.ports[0].host)).unwrap();
        connection
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        connection
            .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut output = String::new();
        connection.read_to_string(&mut output).unwrap();
        assert!(output.starts_with("HTTP/1.0 200"), "{output}");
        serde_json::from_str(output.split_once("\r\n\r\n").unwrap().1).unwrap()
    };
    let wait_health = |id: &str, wanted: pvisor::docker::health::State| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = store.health(id).unwrap().unwrap();
            if status.state == wanted {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "health did not become {wanted:?}: {status:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    compose(&["up", "--wait", "--wait-timeout", "60"], 0);
    let app = store.resolve("compose-health-app-1").unwrap();
    let db = store.resolve("compose-health-db-1").unwrap();
    let migration = store.resolve("compose-health-migration-1").unwrap();
    assert_eq!(migration.exit_code, Some(0));
    assert!(
        store
            .health(&app.id)
            .unwrap()
            .is_some_and(|status| status.state == pvisor::docker::health::State::Healthy)
    );
    assert_eq!(response(&app)["schema"], "v1");
    assert_eq!(response(&app)["message"], "first");
    compose(
        &[
            "exec",
            "-T",
            "--interactive=false",
            "app",
            "python",
            "-c",
            "import socket; assert socket.gethostname() != '(none)'; assert socket.getfqdn('0.0.0.0') != '(none)'",
        ],
        0,
    );
    assert_eq!(fs::read(&writable_file).unwrap(), b"guest-write");
    compose(
        &[
            "exec",
            "-T",
            "--interactive=false",
            "app",
            "python",
            "-c",
            "import glob,os; paths=glob.glob('/run/pvisor-file-*'); assert paths and all(os.listdir(p)==['file'] for p in paths)",
        ],
        0,
    );
    let denied = compose(
        &[
            "exec",
            "-T",
            "--interactive=false",
            "app",
            "python",
            "-c",
            "open('/app/config.json','w')",
        ],
        1,
    );
    let denial = String::from_utf8_lossy(&denied.stderr).to_ascii_lowercase();
    assert!(
        denial.contains("read-only file system") || denial.contains("permission denied"),
        "{denied:?}"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&config_file).unwrap()).unwrap()["message"],
        "first"
    );
    fs::write(
        &config_file,
        r#"{"message":"live-host-update","healthy":false}"#,
    )
    .unwrap();
    assert_eq!(response(&app)["message"], "live-host-update");
    wait_health(&app.id, pvisor::docker::health::State::Unhealthy);
    let unhealthy = compose(&["up", "--wait", "--wait-timeout", "5"], 1);
    assert!(String::from_utf8_lossy(&unhealthy.stderr).contains("unhealthy"));
    fs::write(&config_file, r#"{"message":"recovered","healthy":true}"#).unwrap();
    wait_health(&app.id, pvisor::docker::health::State::Healthy);
    document["services"]["app"]["healthcheck"] = serde_json::json!({
        "test":["CMD","sh","-c","sleep 2; echo leaked > /health-timeout-leaked"],
        "interval":"100ms","timeout":"200ms","retries":1
    });
    fs::write(&file, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    let timed_out = compose(&["up", "--wait", "--wait-timeout", "10"], 1);
    assert!(String::from_utf8_lossy(&timed_out.stderr).contains("timed out"));
    let replacement = store.resolve("compose-health-app-1").unwrap();
    assert_ne!(replacement.generation, app.generation);
    assert_eq!(store.resolve(&db.id).unwrap().id, db.id);
    std::thread::sleep(Duration::from_secs(3));
    compose(
        &[
            "exec",
            "-T",
            "--interactive=false",
            "app",
            "sh",
            "-c",
            "test ! -e /health-timeout-leaked",
        ],
        0,
    );
    document["services"]["app"]["healthcheck"] = check;
    fs::write(&file, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    compose(&["up", "--wait"], 0);
    compose(
        &[
            "exec",
            "-T",
            "--interactive=false",
            "-w",
            "/app",
            "app",
            "python",
            "-c",
            "from client import redis; assert redis('SET','persisted','kept')=='OK'",
        ],
        0,
    );
    let data = store.resolve(&db.id).unwrap().config.mounts[0]
        .source
        .clone();
    compose(&["down", "-t", "2"], 0);
    assert!(data.is_dir());
    compose(&["up", "--wait"], 0);
    compose(
        &[
            "exec",
            "-T",
            "--interactive=false",
            "-w",
            "/app",
            "app",
            "python",
            "-c",
            "from client import redis; assert redis('GET','persisted')=='kept'",
        ],
        0,
    );
    compose(&["down", "-v", "-t", "2"], 0);
    assert!(!data.exists());
    document = serde_json::json!({"name":"compose-health-failure","services":{
        "migration":{"image":python,"command":["python","-c","raise SystemExit(17)"]},
        "app":{"image":python,"command":["python","-c","import time; time.sleep(300)"],
            "depends_on":{"migration":{"condition":"service_completed_successfully"}}}
    }});
    fs::write(&file, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    let failed = compose(&["up", "--wait", "--wait-timeout", "20"], 1);
    assert!(String::from_utf8_lossy(&failed.stderr).contains("17"));
    assert_eq!(
        store
            .resolve("compose-health-failure-migration-1")
            .unwrap()
            .exit_code,
        Some(17)
    );
    let blocked = store.resolve("compose-health-failure-app-1").unwrap();
    assert_eq!(blocked.state, "created");
    assert_eq!(blocked.started_ms, 0);
    compose(&["down"], 0);
    document = serde_json::json!({"name":"compose-health-exec-error","services":{
        "app":{"image":python,"command":["python","-c","import time; time.sleep(300)"],
            "healthcheck":{"test":["CMD","pvisor-no-such-health-command"],
                "interval":"100ms","timeout":"1s","retries":1}}
    }});
    fs::write(&file, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    compose(&["up", "--wait", "--wait-timeout", "20"], 1);
    let failed_check = store.resolve("compose-health-exec-error-app-1").unwrap();
    assert_eq!(
        failed_check.state, "running",
        "failed checks do not kill workloads"
    );
    assert_eq!(
        store.health(&failed_check.id).unwrap().unwrap().exit_code,
        Some(127)
    );
    compose(&["down"], 0);
    document["name"] = "compose-health-deadline".into();
    document["services"]["app"]["healthcheck"] = serde_json::json!({
        "test":["CMD","false"],"interval":"100ms","timeout":"1s","retries":1000
    });
    fs::write(&file, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    let deadline = compose(&["up", "--wait", "--wait-timeout", "1"], 1);
    assert!(String::from_utf8_lossy(&deadline.stderr).contains("readiness timeout"));
    let waiting = store.resolve("compose-health-deadline-app-1").unwrap();
    assert_eq!(waiting.state, "running");
    assert_eq!(
        store.health(&waiting.id).unwrap().unwrap().state,
        pvisor::docker::health::State::Starting
    );
    compose(&["down"], 0);
    let images: Vec<_> = store
        .images()
        .unwrap()
        .into_iter()
        .map(|image| serde_json::json!({"reference":image.reference,"digest":image.digest}))
        .collect();
    let report = serde_json::json!({"case":"compose-health-dependencies-and-file-binds",
        "platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,"images":images,
        "hypervisor":if cfg!(target_os = "linux") { "KVM" } else { "HVF" },
        "checks":["redis readiness","migration completion","service DNS","HTTP publication","valid local hostname",
            "single-file parent isolation","read-only file enforcement","live file visibility","writable file writeback",
            "health failure and recovery after frontend exit","probe timeout kills descendants",
            "generation-fenced health","volume persistence across down/up","down -v cleanup",
            "failed migration blocks dependent startup","missing health command preserves exit 127",
            "readiness deadline"],"outcome":"pass"});
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    if let Some(path) = std::env::var_os("PVISOR_COMPOSE_ACCEPTANCE_REPORT") {
        fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}
