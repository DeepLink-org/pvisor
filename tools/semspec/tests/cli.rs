use semspec::{ledger::Approval, project::Project};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_semspec"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}
fn case(id: &str, script: &str, meta: &str) -> String {
    format!(
        "### S-TEST-{id}: Test\n\n**语义**: Preserve behavior.\n\n**违反示例**: Change behavior.\n\n<!-- semspec: case id=S-TEST-{id} {meta} -->\n```bash\n{script}\n```\n"
    )
}
fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let output = cli(root.path(), &["init", "cases"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    root
}
fn write_cases(root: &Path, text: &str) {
    fs::write(root.join("cases/example.md"), text).unwrap();
}
fn run_json(root: &Path, args: &[&str]) -> (Output, serde_json::Value) {
    let mut arguments = vec!["run", "--format", "json"];
    if args.first().is_none_or(|arg| arg.starts_with("--")) {
        arguments.push("cases");
    };
    arguments.extend(args);
    let output = cli(root, &arguments);
    let json = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
    (output, json)
}
fn clean_report(json: &serde_json::Value) {
    for r in json["results"].as_array().unwrap() {
        if let Some(dir) = r["workdir"].as_str() {
            fs::remove_dir_all(dir).unwrap();
        }
    }
}
#[test]
fn run_selects_markdown_directly_and_intersects_case_selection() {
    let dir = fixture();
    let root = dir.path();
    write_cases(root, &case("001", "true", ""));
    let selected = root.join("cases/selected cases.md");
    fs::write(
        &selected,
        case("002", "true", "") + &case("003", "true", ""),
    )
    .unwrap();
    let (output, json) = run_json(root, &["cases/selected cases.md"]);
    assert!(output.status.success());
    let ids: Vec<_> = json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["S-TEST-002", "S-TEST-003"]);
    let (output, json) = run_json(root, &[selected.to_str().unwrap(), "--case", "S-TEST-003"]);
    assert!(output.status.success());
    assert_eq!(json["results"].as_array().unwrap().len(), 1);
    assert_eq!(json["results"][0]["id"], "S-TEST-003");
    for args in [
        vec!["run", "missing.md"],
        vec!["run", "semspec.toml"],
        vec!["run", "cases/selected cases.md", "--case", "S-TEST-001"],
    ] {
        assert_eq!(cli(root, &args).status.code(), Some(2));
    }
}
#[test]
fn cli_runs_cases_and_keeps_review_separate_from_execution() {
    let dir = fixture();
    let root = dir.path();
    assert_eq!(cli(root, &["init", "cases"]).status.code(), Some(2));
    let text = case(
        "001",
        "printf 'exact\\n' > file\nassert_content file $'exact\\n'\nassert_absent absent",
        "",
    ) + &case("002", "echo failure >&2; exit 9", "")
        + &case("003", "exit 7", "xfail-on=all xfail-reason='known bug'")
        + &case("004", "true", "xfail-on=all xfail-reason='known bug'")
        + &case("005", "echo prerequisite missing >&2; exit 77", "")
        + &case("006", "exit 77", "xfail-on=all xfail-reason='known bug'")
        + &case(
            "007",
            "printf 'value=ok\\n' > library.sh\nsource library.sh\n[ \"$value\" = ok ]\nexpect_exit 77 bash -c 'exit 77'",
            "",
        );
    write_cases(root, &text);
    assert!(cli(root, &["lint", "cases"]).status.success());
    let (output, json) = run_json(root, &[]);
    assert_eq!(output.status.code(), Some(1));
    let verdicts: Vec<_> = json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["verdict"]["verdict"].as_str().unwrap())
        .collect();
    assert_eq!(
        verdicts,
        ["PASS", "FAIL", "XFAIL", "XPASS", "SKIP", "SKIP", "PASS"]
    );
    assert_eq!(json["results"][0]["review"]["state"], "UNREVIEWED");
    assert!(json["results"][0]["workdir"].is_null());
    assert!(json["results"][1]["workdir"].is_string());
    assert!(json["results"][2]["workdir"].is_string());
    assert!(
        json["results"][4]["verdict"]["reason"]
            .as_str()
            .unwrap()
            .contains("prerequisite missing")
    );
    clean_report(&json);
    let (output, json) = run_json(root, &["--case", "S-TEST-006", "--require-reviewed"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(json["results"][0]["verdict"]["verdict"], "SKIP");
    clean_report(&json);
    let (output, json) = run_json(root, &["--case", "S-TEST-001", "--require-reviewed"]);
    assert_eq!(output.status.code(), Some(1));
    clean_report(&json);
    let (output, json) = run_json(root, &["--case", "S-TEST-001", "--keep"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(json["results"][0]["workdir"].is_string());
    clean_report(&json);
    assert_eq!(
        cli(root, &["run", "cases", "--case", "S-TEST-999"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        cli(root, &["run", "cases", "--jobs", "2"]).status.code(),
        Some(2)
    );
}
#[test]
fn changing_text_or_vocabulary_invalidates_fixture_approval_and_snapshots_are_not_cases() {
    let dir = fixture();
    let root = dir.path();
    write_cases(root, &case("001", "true", ""));
    let mut project = Project::load(&[root.join("cases")]).unwrap();
    // Only an ephemeral test ledger: no review CLI or project approval is invoked.
    for id in project.items() {
        let item = project.item(&id).unwrap();
        project.ledger.approve(Approval {
            item: id,
            digest: item.digest,
            reviewer: "fixture".into(),
            date: "2026-10-01".into(),
            signature: None,
        });
    }
    fs::write(
        root.join("cases/REVIEWED.toml"),
        toml::to_string(&project.ledger).unwrap(),
    )
    .unwrap();
    fs::create_dir(root.join("cases/.approved")).unwrap();
    fs::write(
        root.join("cases/.approved/S-TEST-001.md"),
        case("001", "true", ""),
    )
    .unwrap();
    assert_eq!(
        cli(root, &["review", "cases", "--strict"]).status.code(),
        Some(0)
    );
    write_cases(root, &(case("001", "true", "") + "\nAdditional promise."));
    let output = cli(root, &["review", "cases", "--strict"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("STALE"));
    write_cases(root, &case("001", "true", ""));
    fs::write(
        root.join("cases/index.md"),
        fs::read_to_string(root.join("cases/index.md")).unwrap()
            + "\nChanged preparation instructions.\n",
    )
    .unwrap();
    let output = cli(root, &["review", "cases", "--strict"]);
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.lines().filter(|line| line.contains("STALE")).count() >= 2);
}
#[test]
fn timeout_cleans_descendants_and_retains_failure_log() {
    let dir = fixture();
    let root = dir.path();
    write_cases(
        root,
        &case(
            "001",
            "trap 'exit 77' TERM\n(sleep 1; printf escaped > escaped) &\nwait",
            "",
        ),
    );
    let (output, json) = run_json(root, &["--timeout", "50ms"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        json["results"][0]["verdict"]["output_tail"]
            .as_str()
            .unwrap()
            .contains("timed out")
    );
    let workdir = Path::new(json["results"][0]["workdir"].as_str().unwrap());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert!(!workdir.join("ws/escaped").exists());
    clean_report(&json);
}

#[test]
fn infrastructure_errors_are_not_expected_failures() {
    let dir = fixture();
    write_cases(
        dir.path(),
        &case("001", "true", "xfail-on=all xfail-reason='known bug'"),
    );
    let mut project = Project::load(&[dir.path().join("cases")]).unwrap();
    let preparation = project.preparation.remove("index.md").unwrap();
    project
        .preparation
        .insert("invalid\0path".into(), preparation);
    project.cases[0].preparation = vec!["invalid\0path".into()];
    let report = semspec::runner::run(
        &project,
        &project.select(&[], None).unwrap(),
        &semspec::runner::Options {
            timeout: std::time::Duration::from_secs(1),
            subject: None,
            keep: false,
        },
    )
    .unwrap();
    assert_eq!(report.exit_code(&["S-TEST-001"], false, false), 3);
    assert!(matches!(
        report.results[0].verdict,
        semspec::model::Verdict::Error { .. }
    ));
    fs::remove_dir_all(report.results[0].workdir.as_ref().unwrap()).unwrap();
    assert!(
        semspec::runner::execute_process(
            Command::new("/no/interpreter/here"),
            &dir.path().join("case.log"),
            std::time::Duration::from_secs(1),
        )
        .unwrap_err()
        .to_string()
        .contains("launch interpreter")
    );
}

#[test]
fn duplicate_ids_are_parse_errors() {
    let dir = fixture();
    let root = dir.path();
    fs::create_dir(root.join("empty")).unwrap();
    fs::write(
        root.join("empty/tutorial.md"),
        "# Tutorial\n\n```bash\ntrue\n```\n",
    )
    .unwrap();
    assert_eq!(cli(root, &["lint", "empty"]).status.code(), Some(2));
    let text = case("001", "true", "");
    write_cases(root, &(text.clone() + &text));
    assert_eq!(cli(root, &["lint", "cases"]).status.code(), Some(2));
}

#[test]
fn preparation_order_is_deterministic_and_supports_source_return() {
    let dir = fixture();
    let root = dir.path();
    fs::write(
        root.join("cases/index.md"),
        "<!-- semspec: setup -->\n```bash\nvalue=a\nreturn 0\nvalue=wrong\n```\n",
    )
    .unwrap();
    write_cases(
        root,
        &("<!-- semspec: setup -->\n```bash\nvalue=z\n```\n".to_owned()
            + &case("001", "[ \"$value\" = z ]", "")),
    );
    let project = Project::load(&[root.join("cases")]).unwrap();
    assert_eq!(
        project.preparation_names(&project.cases[0]),
        ["index.md", "example.md"]
    );
    let digest = project.item("S-TEST-001").unwrap().digest;
    assert_eq!(
        Project::load(&[root.join("cases")])
            .unwrap()
            .item("S-TEST-001")
            .unwrap()
            .digest,
        digest
    );
    let (output, json) = run_json(root, &[]);
    assert!(output.status.success());
    clean_report(&json);
}

fn document_case(id: &str, script: &str, meta: &str) -> String {
    case(id, script, meta)
}
fn documents_cli(root: &Path, args: &[&str]) -> Output {
    cli(root, args)
}

#[test]
fn markdown_alone_discovers_and_executes_shared_then_local_preparation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let docs = root.join("docs/src/zh/cases");
    fs::create_dir_all(&docs).unwrap();
    fs::write(
        root.join("semspec.toml"),
        "invalid legacy config; never read implicitly",
    )
    .unwrap();
    fs::write(
        docs.join("index.md"),
        "# Tutorial\n\n```bash\nexit 91\n```\n\n<!-- semspec: setup -->\n```bash\nvalue=shared\n```",
    )
    .unwrap();
    fs::write(
        docs.join("01 steps.md"),
        "# Steps\n\n<!-- semspec: setup -->\n```bash\n[ \"$value\" = shared ]\nvalue=local\n```\n\n"
            .to_owned()
            + &document_case(
                "001",
                "[ \"$value\" = local ]\n[ ! -e file ]; printf isolated > file",
                "",
            ) + &document_case("002", "[ \"$value\" = local ]\n[ ! -e file ]", ""),
    )
    .unwrap();
    fs::write(
        docs.join("02-other.md"),
        document_case("003", "[ \"$value\" = shared ]", ""),
    )
    .unwrap();
    assert!(
        documents_cli(root, &["lint", "docs/src/zh/cases"])
            .status
            .success()
    );
    let output = documents_cli(root, &["run", "docs/src/zh/cases", "--format", "json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["results"].as_array().unwrap().len(), 3);
    assert_eq!(json["results"][0]["review"]["state"], "UNREVIEWED");
    let output = documents_cli(
        root,
        &[
            "run",
            "docs/src/zh/cases/01 steps.md",
            "--case",
            "S-TEST-002",
            "--format",
            "json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["results"].as_array().unwrap().len(), 1);
    assert_eq!(json["results"][0]["id"], "S-TEST-002");
    assert_eq!(
        documents_cli(root, &["run", "docs/src/zh/cases", "--require-reviewed"])
            .status
            .code(),
        Some(1)
    );
    let project = Project::load(&[&docs]).unwrap();
    let digest = project.item("S-TEST-001").unwrap().digest;
    // Fixture approvals only; preparation prose is part of the seal too.
    let mut ledger = semspec::ledger::Ledger::default();
    for id in project.items() {
        let item = project.item(&id).unwrap();
        ledger.approve(Approval {
            item: id,
            digest: item.digest,
            reviewer: "fixture".into(),
            date: "2026-10-09".into(),
            signature: None,
        });
    }
    fs::write(
        docs.join("REVIEWED.toml"),
        toml::to_string(&ledger).unwrap(),
    )
    .unwrap();
    assert!(
        documents_cli(root, &["review", "docs/src/zh/cases", "--strict"])
            .status
            .success()
    );
    let index = docs.join("index.md");
    fs::write(
        &index,
        fs::read_to_string(&index).unwrap() + "\nChanged instructions.\n",
    )
    .unwrap();
    let changed = Project::load(&[&docs]).unwrap();
    assert_ne!(changed.item("S-TEST-001").unwrap().digest, digest);
    assert!(matches!(
        changed.item("S-TEST-001").unwrap().review,
        semspec::model::ReviewState::Stale { .. }
    ));
    assert_eq!(
        documents_cli(root, &["review", "docs/src/zh/cases", "--strict"])
            .status
            .code(),
        Some(1)
    );
}

#[test]
fn copied_learning_documents_need_no_repository_scripts_or_config() {
    let dir = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/src/zh/cases");
    for entry in fs::read_dir(source).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "md") {
            fs::copy(&path, dir.path().join(path.file_name().unwrap())).unwrap();
        }
    }
    let output = documents_cli(dir.path(), &["lint", "."]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let project = Project::load(&[dir.path()]).unwrap();
    assert_eq!(project.cases.len(), 30);
    assert_eq!(project.preparation.len(), 2);
    assert!(
        project.preparation["index.md"]
            .script
            .contains("def lookup(")
    );
    assert!(
        project
            .cases
            .iter()
            .filter(|case| case.domain == "USE")
            .all(|case| project.preparation_names(case) == ["index.md"])
    );
}

#[test]
fn init_creates_only_markdown_and_runs_without_a_subject_setting() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        documents_cli(dir.path(), &["init", "cases"])
            .status
            .success()
    );
    assert!(!dir.path().join("semspec.toml").exists());
    assert_eq!(
        documents_cli(dir.path(), &["--config", "semspec.toml", "lint"])
            .status
            .code(),
        Some(2)
    );
    assert!(!dir.path().join("cases/REVIEWED.toml").exists());
    assert_eq!(fs::read_dir(dir.path().join("cases")).unwrap().count(), 2);
    assert_eq!(
        documents_cli(dir.path(), &["init", "cases"]).status.code(),
        Some(2)
    );
    assert!(
        documents_cli(
            dir.path(),
            &["run", "cases", "--subject-bin", "/usr/bin/true"]
        )
        .status
        .success()
    );
    // Cases may use any public command; no subject binary is required by the runner.
    fs::write(
        dir.path().join("cases/example.md"),
        document_case("001", "true", ""),
    )
    .unwrap();
    assert!(
        documents_cli(dir.path(), &["run", "cases"])
            .status
            .success()
    );
    assert_eq!(
        documents_cli(dir.path(), &["run", "cases", "--timeout", "0ms"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        documents_cli(dir.path(), &["run", "missing.md"])
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn comment_parameters_control_verdicts_and_per_case_timeouts() {
    let dir = tempfile::tempdir().unwrap();
    let text = document_case("001", "true", "")
        + &document_case("002", "exit 77", "xfail-on=all xfail-reason='known bug'")
        + &document_case("003", "true", "xfail-on=all xfail-reason='known bug'")
        + &document_case(
            "004",
            "trap 'exit 77' TERM\nsleep 30 &\nwait",
            "timeout=40ms",
        );
    fs::write(dir.path().join("cases.md"), text).unwrap();
    let output = documents_cli(
        dir.path(),
        &["run", "cases.md", "--timeout", "5s", "--format", "json"],
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let verdicts: Vec<_> = json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["verdict"]["verdict"].as_str().unwrap())
        .collect();
    assert_eq!(verdicts, ["PASS", "SKIP", "XPASS", "FAIL"]);
    assert!(
        json["results"][3]["verdict"]["output_tail"]
            .as_str()
            .unwrap()
            .contains("timed out")
    );
    clean_report(&json);
}

#[test]
fn explicit_multiple_inputs_are_a_union_without_path_guessing() {
    let dir = fixture();
    let root = dir.path();
    write_cases(root, &case("001", "true", ""));
    let other = root.join("other cases");
    fs::create_dir(&other).unwrap();
    fs::write(
        other.join("second.md"),
        case("002", "[ \"$value\" = local ]", ""),
    )
    .unwrap();
    fs::write(
        other.join("index.md"),
        "<!-- semspec: setup -->\n```bash\nvalue=local\n```\n",
    )
    .unwrap();
    for args in [
        vec!["run"],
        vec!["lint"],
        vec!["list"],
        vec!["review"],
        vec!["show", "S-TEST-001"],
        vec!["init"],
    ] {
        assert_eq!(cli(root, &args).status.code(), Some(2), "{args:?}");
    }
    let inputs = [
        root.join("cases"),
        other.clone(),
        root.join("cases/example.md"),
    ];
    let project = Project::load(&inputs).unwrap();
    assert_eq!(project.root, root.canonicalize().unwrap());
    assert_eq!(project.cases.len(), 2);
    assert_eq!(
        project.preparation_names(&project.cases[1]),
        ["other cases/index.md"]
    );
    let output = cli(
        root,
        &[
            "run",
            "cases",
            "other cases",
            "cases/example.md",
            "--format",
            "json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["results"].as_array().unwrap().len(), 2);
    assert!(
        cli(root, &["lint", "cases", "other cases"])
            .status
            .success()
    );
    let files = Project::load(&[other.join("second.md"), root.join("cases/example.md")]).unwrap();
    assert_eq!(
        files.item("S-TEST-001").unwrap().digest,
        project.item("S-TEST-001").unwrap().digest
    );
    let output = cli(
        root,
        &[
            "run",
            "cases/example.md",
            "other cases/second.md",
            "--case",
            "S-TEST-002",
            "--format",
            "json",
        ],
    );
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["results"].as_array().unwrap().len(), 1);
    assert_eq!(json["results"][0]["id"], "S-TEST-002");
    let output = cli(
        root,
        &["--spec-dir", "cases", "--spec-dir", "other cases", "list"],
    );
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 2);
    assert!(
        cli(root, &["show", "S-TEST-002", "cases", "other cases"])
            .status
            .success()
    );
    assert_eq!(
        cli(root, &["run", "cases", "missing.md"]).status.code(),
        Some(2)
    );
    fs::write(other.join("duplicate.md"), case("001", "true", "")).unwrap();
    assert_eq!(
        cli(root, &["lint", "cases", "other cases"]).status.code(),
        Some(2)
    );
}

#[test]
fn require_pass_rejects_skips_and_expected_failures_in_selected_cases() {
    let dir = fixture();
    let root = dir.path();
    write_cases(
        root,
        &(case("001", "true", "")
            + &case("002", "exit 77", "")
            + &case("003", "exit 7", "xfail-on=all xfail-reason='known bug'")
            + &case("004", "true", "xfail-on=all xfail-reason='known bug'")
            + &case("005", "exit 9", "")),
    );
    for (id, verdict, code) in [
        ("S-TEST-001", "PASS", 0),
        ("S-TEST-002", "SKIP", 1),
        ("S-TEST-003", "XFAIL", 1),
        ("S-TEST-004", "XPASS", 1),
        ("S-TEST-005", "FAIL", 1),
    ] {
        let (output, json) = run_json(root, &["--case", id, "--require-pass"]);
        assert_eq!(output.status.code(), Some(code));
        assert_eq!(json["results"].as_array().unwrap().len(), 1);
        assert_eq!(json["results"][0]["id"], id);
        assert_eq!(json["results"][0]["verdict"]["verdict"], verdict);
        clean_report(&json);
    }
    let (output, json) = run_json(
        root,
        &[
            "--case",
            "S-TEST-001",
            "--require-pass",
            "--require-reviewed",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(json["results"][0]["verdict"]["verdict"], "PASS");
    assert!(!root.join("cases/REVIEWED.toml").exists());
    clean_report(&json);
}

#[test]
fn report_lifecycle_preserves_invalid_selections_and_clears_stale_results_before_execution() {
    let dir = fixture();
    let root = dir.path();
    write_cases(root, &case("001", "true", ""));
    fs::write(
        root.join("cases/other.md"),
        case("001", "true", "").replace("S-TEST-001", "S-OTHER-001"),
    )
    .unwrap();
    let path = root.join("report with spaces.json");
    let old = b"old successful report";
    for selection in ["S-TEST-999", "S-TEST-001,S-TEST-001", "S-OTHER-001"] {
        fs::write(&path, old).unwrap();
        let output = cli(
            root,
            &[
                "run",
                "cases",
                "--domain",
                "TEST",
                "--case",
                selection,
                "--require-pass",
                "--output",
                path.to_str().unwrap(),
            ],
        );
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(fs::read(&path).unwrap(), old);
    }
    let output = cli(
        root,
        &[
            "run",
            "cases",
            "--case",
            "S-TEST-001",
            "--require-pass",
            "--subject-bin",
            "missing-subject",
            "--output",
            path.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(!path.exists());
    for (script, verdict, code) in [("true", "PASS", 0), ("exit 9", "FAIL", 1)] {
        write_cases(root, &case("001", script, ""));
        fs::write(&path, old).unwrap();
        let output = cli(
            root,
            &[
                "run",
                "cases",
                "--case",
                "S-TEST-001",
                "--require-pass",
                "--format",
                "json",
                "--output",
                path.to_str().unwrap(),
            ],
        );
        assert_eq!(output.status.code(), Some(code));
        let json: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(json["results"].as_array().unwrap().len(), 1);
        assert_eq!(json["results"][0]["id"], "S-TEST-001");
        assert_eq!(json["results"][0]["verdict"]["verdict"], verdict);
        clean_report(&json);
    }
}

#[test]
fn report_output_cannot_replace_specifications_preparation_or_fixture_ledgers() {
    let dir = fixture();
    let root = dir.path();
    write_cases(root, &case("001", "true", ""));
    fs::write(root.join("cases/REVIEWED.toml"), "format = 1\n").unwrap();
    for output in ["cases/example.md", "cases/index.md", "cases/REVIEWED.toml"] {
        let before = fs::read(root.join(output)).unwrap();
        assert_eq!(
            cli(root, &["run", "cases", "--output", output])
                .status
                .code(),
            Some(2)
        );
        assert_eq!(fs::read(root.join(output)).unwrap(), before);
    }
}
