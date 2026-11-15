//! Real guest acceptance of restart ownership, generation waits and file streams.
use super::*;

fn observe(
    native: &Native,
    predicate: impl Fn(&pvisor::docker::Instance) -> bool,
) -> pvisor::docker::Instance {
    let store = pvisor::docker::Store::open(native.home.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let instance = store.resolve("native-test").unwrap();
        if predicate(&instance) {
            return instance;
        }
        assert!(Instant::now() < deadline, "last state: {instance:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "requires Linux/KVM or Apple Silicon/HVF, firmware and a native Alpine fixture"]
fn docker_native_p0_on_failure_bounds_restarts_and_wait_returns_exit_as_data() {
    let native = Native::new();
    native.call(&["run","-d","--name","native-test","--cpus","1","-m","64m",
        "--restart","on-failure:2","--label","role=worker","fixture:latest",
        "/bin/sh","-c",
        "n=$(cat /counter 2>/dev/null || echo 0); n=$((n+1)); echo \"$n\" >/counter; echo \"$n\"; exit 7"],
        None,0);
    let output = native.call(&["wait", "native-test"], None, 0);
    assert_eq!(output.stdout, b"7\n");
    let final_state = observe(&native, |i| i.restart_count == 2 && i.state == "exited");
    assert_eq!(final_state.exit_code, Some(7));
    // Wait for the continuously held owner lease to release, not just the
    // intermediate exited projection published between automatic generations.
    let deadline = Instant::now() + Duration::from_secs(5);
    let path = native._temporary.path().join("counter");
    loop {
        let output = Command::new(&native.binary)
            .env("PVISOR_DOCKER_HOME", &native.home)
            .args(["cp", "native-test:/counter"])
            .arg(&path)
            .output()
            .unwrap();
        if output.status.success() {
            break;
        }
        assert!(Instant::now() < deadline, "{output:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(fs::read(path).unwrap(), b"3\n");
    let metadata = native.call(&["container", "inspect", "native-test"], None, 0);
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout).unwrap();
    assert_eq!(metadata[0]["RestartCount"], 2);
    assert_eq!(metadata[0]["Config"]["Labels"]["role"], "worker");
    assert_eq!(
        native.call(&["logs", "native-test"], None, 0).stdout,
        b"1\n2\n3\n"
    );
}

#[test]
#[ignore = "requires Linux/KVM or Apple Silicon/HVF, firmware and a native Alpine fixture"]
fn docker_native_p0_manual_stop_suppresses_always_and_manual_restart_preserves_files() {
    let native = Native::new();
    native.call(
        &[
            "run",
            "-d",
            "--name",
            "native-test",
            "--cpus",
            "1",
            "-m",
            "64m",
            "--restart",
            "always",
            "fixture:latest",
            "/bin/sh",
            "-c",
            "echo boot >>/boots; sleep 0.2; exit 0",
        ],
        None,
        0,
    );
    observe(&native, |i| i.restart_count >= 1);
    native.call(&["stop", "-t", "0", "native-test"], None, 0);
    let stopped = observe(&native, |i| i.state == "exited");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(observe(&native, |_| true).generation, stopped.generation);
    native.call(&["container", "restart", "-t", "0", "native-test"], None, 0);
    let restarted = observe(&native, |i| i.generation != stopped.generation);
    assert_ne!(restarted.generation, stopped.generation);
    native.call(&["stop", "-t", "0", "native-test"], None, 0);
    let path = native._temporary.path().join("boots");
    native.call(
        &["cp", "native-test:/boots", path.to_str().unwrap()],
        None,
        0,
    );
    assert!(fs::read_to_string(path).unwrap().lines().count() >= 2);
}

#[test]
#[ignore = "requires Linux/KVM or Apple Silicon/HVF, firmware and a native Alpine fixture"]
fn docker_native_p0_live_copy_preserves_binary_streams_and_ro_mount_authority() {
    let native = Native::new();
    let source = native._temporary.path().join("input");
    let bytes: Vec<_> = (0..=255).cycle().take(1024 * 1024 + 17).collect();
    fs::write(&source, &bytes).unwrap();
    let mount = native._temporary.path().join("readonly");
    fs::create_dir(&mount).unwrap();
    fs::write(mount.join("keep"), b"keep").unwrap();
    native.call(
        &[
            "run",
            "-d",
            "--name",
            "native-test",
            "--cpus",
            "1",
            "-m",
            "64m",
            "-v",
            &format!("{}:/readonly:ro", mount.display()),
            "fixture:latest",
            "/bin/sleep",
            "60",
        ],
        None,
        0,
    );
    native.call(
        &["cp", source.to_str().unwrap(), "native-test:/binary"],
        None,
        0,
    );
    let output = native._temporary.path().join("output");
    native.call(
        &[
            "container",
            "cp",
            "native-test:/binary",
            output.to_str().unwrap(),
        ],
        None,
        0,
    );
    assert_eq!(fs::read(&output).unwrap(), bytes);
    native.call(
        &["cp", source.to_str().unwrap(), "native-test:/readonly/keep"],
        None,
        1,
    );
    assert_eq!(fs::read(mount.join("keep")).unwrap(), b"keep");
    native.call(&["stop", "-t", "0", "native-test"], None, 0);
    native.call(
        &["cp", "native-test:/binary", output.to_str().unwrap()],
        None,
        0,
    );
    assert_eq!(fs::read(output).unwrap(), bytes);
    let staged = native._temporary.path().join("staged");
    fs::write(&staged, b"offline update").unwrap();
    native.call(
        &["cp", staged.to_str().unwrap(), "native-test:/offline"],
        None,
        0,
    );
    native.call(&["restart", "-t", "0", "native-test"], None, 0);
    assert_eq!(
        native
            .call(&["exec", "native-test", "/bin/cat", "/offline"], None, 0)
            .stdout,
        b"offline update"
    );
}

#[test]
#[ignore = "requires Linux/KVM or Apple Silicon/HVF, firmware and a native Alpine fixture"]
fn docker_native_p0_compose_honors_stop_signal_and_grace_without_restarting() {
    let native = Native::new();
    let file = native._temporary.path().join("compose.yaml");
    fs::write(&file,r#"
name: p0
services:
  app:
    image: fixture:latest
    cpus: 1
    mem_limit: 64m
    restart: always
    stop_signal: SIGUSR1
    stop_grace_period: 2s
    labels: {role: service}
    command: ["/bin/sh", "-c", "trap 'echo graceful >/stopped; exit 0' USR1; echo ready >/ready; while :; do sleep 0.1; done"]
"#).unwrap();
    struct Cleanup<'a>(&'a Native, &'a Path);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = Command::new(&self.0.binary)
                .env("PVISOR_DOCKER_HOME", &self.0.home)
                .args(["compose", "-f"])
                .arg(self.1)
                .args(["down", "-t", "0", "-v"])
                .output();
        }
    }
    let _cleanup = Cleanup(&native, &file);
    let prefix = ["compose", "-f", file.to_str().unwrap()];
    native.call(&[&prefix[..], &["up", "--wait"]].concat(), None, 0);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let output = Command::new(&native.binary)
            .env("PVISOR_DOCKER_HOME", &native.home)
            .args(["exec", "p0-app-1", "/bin/cat", "/ready"])
            .output()
            .unwrap();
        if output.status.success() && output.stdout == b"ready\n" {
            break;
        }
        assert!(Instant::now() < deadline, "{output:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    native.call(&[&prefix[..], &["stop"]].concat(), None, 0);
    let store = pvisor::docker::Store::open(native.home.clone()).unwrap();
    let stopped = store.resolve("p0-app-1").unwrap();
    assert_eq!(stopped.exit_code, Some(0));
    assert_eq!(stopped.config.stop_timeout_ms, 2000);
    assert_eq!(stopped.config.labels["com.docker.compose.project"], "p0");
    assert_eq!(stopped.config.labels["role"], "service");
    let output = native._temporary.path().join("stopped");
    native.call(
        &["cp", "p0-app-1:/stopped", output.to_str().unwrap()],
        None,
        0,
    );
    assert_eq!(fs::read(output).unwrap(), b"graceful\n");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        store.resolve("p0-app-1").unwrap().generation,
        stopped.generation
    );
}

#[test]
#[ignore = "requires Linux/KVM or Apple Silicon/HVF, firmware and a native Alpine fixture"]
fn compose_native_session_retry_waits_for_health_before_starting_dependents() {
    let native = Native::new();
    let file = native._temporary.path().join("compose.yaml");
    fs::write(&file,r#"
name: retry-session
services:
  db:
    image: fixture:latest
    cpus: 1
    mem_limit: 64m
    restart: on-failure:1
    command: ["/bin/sh", "-c", "if [ ! -f /attempt ]; then touch /attempt; sleep 1; exit 7; fi; touch /ready; exec sleep 60"]
    healthcheck:
      test: ["CMD", "test", "-f", "/ready"]
      interval: 100ms
      timeout: 1s
      retries: 1000
  app:
    image: fixture:latest
    cpus: 1
    mem_limit: 64m
    depends_on:
      db: {condition: service_healthy}
    command: ["sleep", "60"]
"#).unwrap();
    struct Cleanup<'a>(&'a Native, &'a Path);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = Command::new(&self.0.binary)
                .env("PVISOR_DOCKER_HOME", &self.0.home)
                .args(["compose", "-f"])
                .arg(self.1)
                .args(["down", "-t", "0", "-v"])
                .output();
        }
    }
    let _cleanup = Cleanup(&native, &file);
    native.call(
        &[
            "compose",
            "-f",
            file.to_str().unwrap(),
            "up",
            "--wait",
            "--wait-timeout",
            "15",
        ],
        None,
        0,
    );
    let store = pvisor::docker::Store::open(native.home.clone()).unwrap();
    let db = store.resolve("retry-session-db-1").unwrap();
    assert_eq!(db.restart_count, 1);
    assert_ne!(db.owner_epoch, db.generation);
    assert_eq!(
        store.health(&db.id).unwrap().unwrap().state,
        pvisor::docker::health::State::Healthy
    );
    assert_eq!(
        store.resolve("retry-session-app-1").unwrap().state,
        "running"
    );
    let session = store.session(&db.id).unwrap();
    assert_eq!(session.run_id(), db.id);
    assert_eq!(session.owner_epoch(), db.owner_epoch);
    assert_eq!(session.active_execution(), db.generation);
}

#[test]
#[ignore = "requires Linux/KVM or Apple Silicon/HVF, firmware and a native Alpine fixture"]
fn docker_native_session_inherits_image_health_and_can_explicitly_disable_it() {
    let native = Native::new();
    let context = native._temporary.path().join("health-image");
    fs::create_dir(&context).unwrap();
    fs::write(context.join("Dockerfile"),
        "FROM fixture:latest\nHEALTHCHECK --interval=100ms --timeout=1s --retries=3 CMD [\"test\", \"-f\", \"/ready\"]\nCMD [\"sleep\", \"60\"]\n").unwrap();
    native.call(
        &["build", "-t", "health:test", context.to_str().unwrap()],
        None,
        0,
    );
    native.call(
        &[
            "run",
            "-d",
            "--name",
            "native-test",
            "--cpus",
            "1",
            "-m",
            "64m",
            "health:test",
        ],
        None,
        0,
    );
    let store = pvisor::docker::Store::open(native.home.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if store.health("native-test").unwrap().unwrap().state
            == pvisor::docker::health::State::Unhealthy
        {
            break;
        }
        assert!(Instant::now() < deadline, "image check did not run");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        store.resolve("native-test").unwrap().state,
        "running",
        "unhealthy does not terminate the workload"
    );
    native.call(&["exec", "native-test", "touch", "/ready"], None, 0);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if store.health("native-test").unwrap().unwrap().state
            == pvisor::docker::health::State::Healthy
        {
            break;
        }
        assert!(Instant::now() < deadline, "health did not recover");
        std::thread::sleep(Duration::from_millis(20));
    }
    native.call(&["rm", "-f", "native-test"], None, 0);
    native.call(
        &[
            "run",
            "-d",
            "--name",
            "native-test",
            "--cpus",
            "1",
            "-m",
            "64m",
            "--no-healthcheck",
            "health:test",
        ],
        None,
        0,
    );
    assert!(store.health("native-test").unwrap().is_none());
}
