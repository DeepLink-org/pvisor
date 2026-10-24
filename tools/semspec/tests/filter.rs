use semspec::{project::Project, seal};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_semspec"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}
fn case(script: &str) -> String {
    format!(
        "---\ntitle: Tutorial\n---\n# Example\n\n[link](other.md)\n\n<!-- semspec: case id=S-TEST-001 -->\n```bash\n{script}\n```\n\nExpected stdout:\n\n<!-- semspec: expect case=S-TEST-001 stream=stdout -->\n```text\nhello\n```\n"
    )
}
fn clean(root: &Path) {
    if let Ok(bytes) = fs::read(root.join("evidence/report.json")) {
        let report: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        for row in report["results"].as_array().unwrap() {
            if let Some(path) = row["workdir"].as_str() {
                fs::remove_dir_all(path).unwrap();
            }
        }
    }
}
#[test]
fn filter_is_read_only_and_preserves_opaque_markdown() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let text = case(&format!(
        "touch '{}'\nprintf 'hello\\n'",
        root.join("executed").display()
    )) + "\n````markdown\n<!-- semspec: case id=S-FAKE-001 -->\n```bash\nfalse\n```\n````\n";
    fs::write(root.join("case.md"), &text).unwrap();
    let out = cli(root, &["filter", "case.md"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8(out.stdout).unwrap();
    assert!(!root.join("executed").exists());
    assert!(md.starts_with("---\ntitle: Tutorial\n---\n# Example\n\n[link](other.md)"));
    assert!(md.contains("S-TEST-001 — NOT RUN"));
    assert!(md.contains("<!-- semspec: case id=S-FAKE-001 -->"));
    assert_eq!(fs::read_to_string(root.join("case.md")).unwrap(), text);
    assert_eq!(fs::read_dir(root).unwrap().count(), 1);
    assert_eq!(
        cli(root, &["filter", "case.md", "--strict"]).status.code(),
        Some(2)
    );
}
#[test]
fn execute_renders_actual_streams_and_exact_expectations_without_mandatory_results() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(
        root.join("case.md"),
        case("printf 'hello\\n'; printf 'diagnostic\\n' >&2"),
    )
    .unwrap();
    let out = cli(root, &["filter", "case.md", "--execute"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8(out.stdout).unwrap();
    assert!(md.contains("S-TEST-001 — PASS"));
    assert!(md.contains("case review: UNREVIEWED"));
    assert!(md.contains("stdout:\n\n```text\nhello\n```"));
    assert!(md.contains("stderr:\n\n```text\ndiagnostic\n```"));
    assert_eq!(fs::read_dir(root).unwrap().count(), 1);
    fs::write(root.join("case.md"), case("printf wrong")).unwrap();
    let out = cli(
        root,
        &["filter", "case.md", "--execute", "--artifacts", "evidence"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("S-TEST-001 — FAIL"));
    clean(root);
}
#[test]
fn offline_results_bind_prose_setup_and_engine_and_never_approve() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("case.md"), case("printf 'hello\\n'")).unwrap();
    fs::write(
        root.join("index.md"),
        "# Setup\n<!-- semspec: setup -->\n```bash\ntrue\n```\n",
    )
    .unwrap();
    assert!(
        cli(
            root,
            &["filter", "case.md", "--execute", "--artifacts", "evidence"]
        )
        .status
        .success()
    );
    assert!(!root.join("REVIEWED.toml").exists());
    assert!(
        cli(
            root,
            &["filter", "case.md", "--results", "evidence", "--strict"]
        )
        .status
        .success()
    );
    let path = root.join("index.md");
    let original = fs::read_to_string(&path).unwrap();
    fs::write(&path, original.clone() + "\nChanged setup prose\n").unwrap();
    let out = cli(root, &["filter", "case.md", "--results", "evidence"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("S-TEST-001 — STALE"));
    assert!(
        !cli(
            root,
            &["filter", "case.md", "--results", "evidence", "--strict"]
        )
        .status
        .success()
    );
    fs::write(&path, original).unwrap();
    let report_path = root.join("evidence/report.json");
    let mut report: serde_json::Value =
        serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    report["engine_semantics"] = "6".into();
    fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();
    assert!(
        !cli(
            root,
            &["filter", "case.md", "--results", "evidence", "--strict"]
        )
        .status
        .success()
    );
    clean(root);
}
#[test]
fn stdin_json_exposes_exact_edits_and_dependencies_and_blocks_altered_execution() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let text = case("printf 'hello\\n'") + "\n{{ table(\"data.json\") }}\n";
    fs::write(root.join("case.md"), &text).unwrap();
    fs::write(root.join("data.json"), "{\"row\":{\"N\":2}}").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_semspec"))
        .current_dir(root)
        .args([
            "filter", "--source", "case.md", "--stdin", "--format", "json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        json["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().ends_with("data.json"))
    );
    let mut rendered = text.clone();
    for edit in json["edits"].as_array().unwrap().iter().rev() {
        rendered.replace_range(
            edit["start"].as_u64().unwrap() as usize..edit["end"].as_u64().unwrap() as usize,
            edit["text"].as_str().unwrap(),
        );
    }
    assert_eq!(rendered, json["markdown"].as_str().unwrap());
    let mut child = Command::new(env!("CARGO_BIN_EXE_semspec"))
        .current_dir(root)
        .args(["filter", "--source", "case.md", "--stdin", "--execute"])
        .stdin(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.replace("printf", "echo").as_bytes())
        .unwrap();
    assert!(!child.wait().unwrap().success());
}
#[test]
fn output_guards_include_nonexistent_ledger_and_symlink_ancestors() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("case.md"), case("printf 'hello\\n'")).unwrap();
    fs::write(
        root.join("index.md"),
        "<!-- semspec: setup -->\n```bash\ntrue\n```\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(root, root.join("alias")).unwrap();
    for path in [
        "case.md",
        "index.md",
        "REVIEWED.toml",
        "alias/REVIEWED.toml",
        "alias/case.md",
    ] {
        let out = cli(root, &["filter", "case.md", "--output", path]);
        assert_eq!(out.status.code(), Some(2), "{path}");
    }
    assert!(!root.join("REVIEWED.toml").exists());
    fs::write(root.join("data.json"), "{\"row\":{\"N\":2}}").unwrap();
    let text = case("printf 'hello\\n'") + "\n{{ table(\"data.json\") }}\n";
    fs::write(root.join("case.md"), text).unwrap();
    assert_eq!(
        cli(root, &["filter", "case.md", "--output", "data.json"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        fs::read_to_string(root.join("data.json")).unwrap(),
        "{\"row\":{\"N\":2}}"
    );
    assert_eq!(
        cli(
            root,
            &[
                "filter",
                "case.md",
                "--execute",
                "--artifacts",
                "evidence",
                "--output",
                "evidence/report.json"
            ]
        )
        .status
        .code(),
        Some(2)
    );
    assert!(!root.join("evidence/report.json").exists());
}
#[test]
fn execution_digest_binds_significant_heredoc_and_setup_whitespace_and_root_is_stable() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir(root.join("sub")).unwrap();
    let path = root.join("sub/case.md");
    fs::write(&path, case("cat <<'END'\nhello \nEND")).unwrap();
    let before = Project::load_at(&[&path], Some(root)).unwrap();
    let digest = before.case_digest(&before.cases[0]);
    fs::write(&path, case("cat <<'END'\nhello\nEND")).unwrap();
    let after = Project::load_at(&[&path], Some(root)).unwrap();
    assert_ne!(digest, after.case_digest(&after.cases[0]));
    assert_eq!(after.root, root.canonicalize().unwrap());
    let setup = root.join("sub/index.md");
    fs::write(
        &setup,
        "<!-- semspec: setup -->\n```bash\nvalue=$(cat <<'END'\nhello \nEND\n)\n```\n",
    )
    .unwrap();
    let with_setup = Project::load_at(&[&path], Some(root)).unwrap();
    fs::write(
        &setup,
        fs::read_to_string(&setup)
            .unwrap()
            .replace("hello \n", "hello\n"),
    )
    .unwrap();
    let changed_setup = Project::load_at(&[&path], Some(root)).unwrap();
    assert_ne!(
        with_setup.case_digest(&with_setup.cases[0]),
        changed_setup.case_digest(&changed_setup.cases[0])
    );
    let directory = Project::load_at(&[root], Some(root)).unwrap();
    assert_eq!(
        directory.case_digest(&directory.cases[0]),
        changed_setup.case_digest(&changed_setup.cases[0])
    );
    assert_ne!(
        seal::execution_digest("prose", "hello \n"),
        seal::execution_digest("prose", "hello\n")
    );
}
#[test]
fn expected_failures_cannot_hide_timeouts_or_missing_commands() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    for script in ["sleep 30", "semspec_nonexistent_command"] {
        let text = format!(
            "# Known issue\n<!-- semspec: case id=S-TEST-001 timeout=30ms xfail-on=all xfail-reason=issue -->\n```bash\n{script}\n```\n"
        );
        fs::write(root.join("case.md"), text).unwrap();
        let out = cli(
            root,
            &["filter", "case.md", "--execute", "--artifacts", "evidence"],
        );
        assert_eq!(out.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&out.stdout).contains("S-TEST-001 — FAIL"));
        clean(root);
        fs::remove_dir_all(root.join("evidence")).unwrap();
    }
}
#[test]
fn cli_sigterm_cleans_owned_children_and_returns_signal_exit() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let pid_path = root.join("pid");
    let proof = root.join("orphan");
    let script = format!(
        "(sleep 2; touch '{}') &\necho $! > '{}'\nwait",
        proof.display(),
        pid_path.display()
    );
    fs::write(root.join("case.md"), case(&script)).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_semspec"))
        .current_dir(root)
        .args(["filter", "case.md", "--execute", "--artifacts", "evidence"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !pid_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(pid_path.exists());
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    assert_eq!(child.wait().unwrap().code(), Some(143));
    std::thread::sleep(Duration::from_millis(2100));
    assert!(!proof.exists());
    clean(root);
}
#[test]
fn markdown_bench_uses_shared_runtime_and_retains_hooks_and_raw_trials() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let text = "# Launch\n\nA local launch observation.\n\n<!-- semspec: bench id=local-launch -->\n```toml\nargv = [\"python3\", \"-c\", \"print('hello')\"]\nwarmup = 1\nruns = 2\ntimeout = 5\nexpected_stdout = \"hello\\n\"\nprepare = [\"python3\", \"-c\", \"pass\"]\nconclude = [\"python3\", \"-c\", \"pass\"]\ncleanup = [\"python3\", \"-c\", \"pass\"]\n```\n";
    fs::write(root.join("bench.md"), text).unwrap();
    assert!(
        String::from_utf8_lossy(&cli(root, &["filter", "bench.md"]).stdout).contains("NOT RUN")
    );
    let out = cli(
        root,
        &["filter", "bench.md", "--execute", "--artifacts", "evidence"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8(out.stdout).unwrap();
    assert!(md.contains("local-launch — PASS"));
    assert!(md.contains("| N | Median ms |"));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("evidence/report.json")).unwrap()).unwrap();
    let data = &report["benchmarks"][0]["data"];
    assert_eq!(data["engine"], "python-perf_counter_ns");
    assert_eq!(data["samples"].as_array().unwrap().len(), 2);
    assert_eq!(data["trials"].as_array().unwrap().len(), 3);
    assert_eq!(data["trials"][0]["hooks"].as_array().unwrap().len(), 3);
    assert!(
        cli(
            root,
            &["filter", "bench.md", "--results", "evidence", "--strict"]
        )
        .status
        .success()
    );
    fs::write(root.join("bench.md"), text.replace("runs = 2", "runs = 3")).unwrap();
    assert!(
        !cli(
            root,
            &["filter", "bench.md", "--results", "evidence", "--strict"]
        )
        .status
        .success()
    );
}

#[test]
fn empty_expectations_are_empty_bytes_and_invalid_expectations_fail_before_execution() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let text = "# Empty stream\n<!-- semspec: case id=S-TEST-001 -->\n```bash\ntrue\n```\n<!-- semspec: expect case=S-TEST-001 stream=stdout -->\n```text\n```\n";
    fs::write(root.join("case.md"), text).unwrap();
    let out = cli(root, &["filter", "case.md", "--execute"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for malformed in [
        text.replace("stream=stdout", "stream=other"),
        text.replace("expect case=S-TEST-001", "expect case=S-TEST-002"),
        format!("{text}<!-- semspec: expect case=S-TEST-001 stream=stdout -->\n```text\n```\n"),
    ] {
        fs::write(root.join("case.md"), malformed).unwrap();
        assert_eq!(
            cli(root, &["filter", "case.md", "--execute"]).status.code(),
            Some(2)
        );
    }
}

#[test]
fn stream_preview_cannot_escape_its_fence_and_marks_truncation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let text = r#"# Output
<!-- semspec: case id=S-TEST-001 -->
```bash
python3 -c 'print("`" * 5 + "<script>" + "x" * 70000)'
```
"#;
    fs::write(root.join("case.md"), text).unwrap();
    let out = cli(root, &["filter", "case.md", "--execute"]);
    assert!(out.status.success());
    let md = String::from_utf8(out.stdout).unwrap();
    assert!(md.contains("stdout:\n\n``````text\n`````<script>"));
    assert!(md.contains("Output preview truncated"));
}

#[test]
fn failure_diagnostics_retain_end_of_large_streams() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(
        root.join("case.md"),
        r#"# Failure
<!-- semspec: case id=S-TEST-001 -->
```bash
python3 -c 'import sys; print("x" * 100000 + "FINAL_FAILURE"); sys.exit(1)'
```
"#,
    )
    .unwrap();
    let out = cli(
        root,
        &["filter", "case.md", "--execute", "--artifacts", "evidence"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("FINAL_FAILURE"));
    clean(root);
}

#[test]
fn process_markdown_profiles_preserve_registered_workloads_and_sampling() {
    use std::collections::BTreeMap;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark/process");
    for (profile, warmup, runs) in [("smoke", 2, 10), ("nightly", 10, 50)] {
        let source = fs::read_to_string(root.join(format!("{profile}.md"))).unwrap();
        let cells = semspec::experiment::cells(&source).unwrap();
        assert_eq!(
            cells.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["native", "host-run", "status", "review"]
        );
        let mut controls = BTreeMap::new();
        for cell in cells {
            assert_eq!((cell.plan.warmup, cell.plan.runs), (warmup, runs));
            assert!(cell.plan.prepare.is_empty() && cell.plan.cleanup.is_empty());
            assert!(cell.plan.cwd.is_none() && cell.plan.expected_stdout.is_none());
            let value = serde_json::to_value(&cell.plan).unwrap();
            let control: BTreeMap<_, _> = ["argv", "env", "conclude", "timeout"]
                .into_iter()
                .map(|key| (key, value[key].clone()))
                .collect();
            controls.insert(cell.id, control);
        }
        assert_eq!(
            seal::digest(&[&serde_json::to_vec(&controls).unwrap()]),
            "sha256:05130ac765f89afd6676af5578a1667f130257db5b01e5f9fe4b2cb423c0bb3b"
        );
        assert!(!root.join(format!("{profile}.json")).exists());
        assert!(!root.join(format!("{profile}.commands.json")).exists());
    }
}

#[test]
fn filter_inventory_exposes_validated_plans_without_executing_them() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("bench.md"), "# Read only\n<!-- semspec: bench id=probe -->\n```toml\nargv = [\"touch\", \"executed\"]\nruns = 2\n```\n").unwrap();
    let out = cli(root, &["filter", "bench.md", "--format", "json"]);
    assert!(out.status.success());
    let document: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let cell = &document["experiments"][0];
    assert_eq!(cell["id"], "probe");
    assert_eq!(
        cell["plan"]["argv"],
        serde_json::json!(["touch", "executed"])
    );
    assert_eq!(cell["plan"]["runs"], 2);
    assert_eq!(cell["plan"]["warmup"], 0);
    assert!(cell["end"].as_u64().unwrap() > 0);
    assert!(cell["digest"].as_str().unwrap().starts_with("sha256:"));
    assert!(!root.join("executed").exists());
}
