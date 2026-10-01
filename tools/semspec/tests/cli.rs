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
        "### S-TEST-{id}: Test\n\n**语义**: Preserve behavior.\n\n**违反示例**: Change behavior.\n\n{meta}\n\n```bash\n{script}\n```\n"
    )
}
fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let output = cli(root.path(), &["init"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    root
}
fn write_cases(root: &Path, text: &str) {
    fs::write(root.join("semantics/example.md"), text).unwrap();
}
fn run_json(root: &Path, args: &[&str]) -> (Output, serde_json::Value) {
    let mut arguments = vec!["run", "--format", "json"];
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
fn cli_runs_cases_and_keeps_review_separate_from_execution() {
    let dir = fixture();
    let root = dir.path();
    assert_eq!(cli(root, &["init"]).status.code(), Some(2));
    let text = case(
        "001",
        "printf 'exact\\n' > file\nassert_content file $'exact\\n'\nassert_absent absent",
        "",
    ) + &case("002", "echo failure >&2; exit 9", "")
        + &case(
            "003",
            "exit 7",
            "<!-- semantic-case: xfail-on=all xfail-reason='known bug' -->",
        )
        + &case(
            "004",
            "true",
            "<!-- semantic-case: xfail-on=all xfail-reason='known bug' -->",
        )
        + &case("005", "exit 1", "<!-- semantic-case: requires=absent -->");
    write_cases(root, &text);
    let config_path = root.join("semspec.toml");
    let mut config = fs::read_to_string(&config_path).unwrap();
    config.push_str(&format!(
        "\n[requirements.absent.{}]\npath_exists = '/does/not/exist'\n",
        std::env::consts::OS
    ));
    // The default platform name is macos on macOS and linux on Linux.
    fs::write(&config_path, config).unwrap();
    assert!(cli(root, &["lint"]).status.success());
    let (output, json) = run_json(root, &[]);
    assert_eq!(output.status.code(), Some(1));
    let verdicts: Vec<_> = json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["verdict"]["verdict"].as_str().unwrap())
        .collect();
    assert_eq!(verdicts, ["PASS", "FAIL", "XFAIL", "XPASS", "SKIP"]);
    assert_eq!(json["results"][0]["review"]["state"], "UNREVIEWED");
    assert!(json["results"][0]["workdir"].is_null());
    assert!(json["results"][1]["workdir"].is_string());
    assert!(json["results"][2]["workdir"].is_string());
    clean_report(&json);
    let (output, json) = run_json(root, &["--case", "S-TEST-001", "--require-reviewed"]);
    assert_eq!(output.status.code(), Some(1));
    clean_report(&json);
    let (output, json) = run_json(root, &["--case", "S-TEST-001", "--keep"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(json["results"][0]["workdir"].is_string());
    clean_report(&json);
    assert_eq!(
        cli(root, &["run", "--case", "S-TEST-999"]).status.code(),
        Some(2)
    );
    assert_eq!(cli(root, &["run", "--jobs", "2"]).status.code(), Some(2));
}
#[test]
fn changing_text_or_vocabulary_invalidates_fixture_approval_and_snapshots_are_not_cases() {
    let dir = fixture();
    let root = dir.path();
    write_cases(root, &case("001", "true", ""));
    let mut project = Project::load(&root.join("semspec.toml")).unwrap();
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
        root.join("semantics/REVIEWED.toml"),
        toml::to_string(&project.ledger).unwrap(),
    )
    .unwrap();
    fs::create_dir(root.join("semantics/.approved")).unwrap();
    fs::write(
        root.join("semantics/.approved/S-TEST-001.md"),
        case("001", "true", ""),
    )
    .unwrap();
    assert_eq!(cli(root, &["review", "--strict"]).status.code(), Some(0));
    write_cases(root, &case("001", "true", "\nAdditional promise."));
    let output = cli(root, &["review", "--strict"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("STALE"));
    write_cases(root, &case("001", "true", ""));
    fs::write(
        root.join("semantics/vocab/core.sh"),
        "# changed vocabulary\n",
    )
    .unwrap();
    let output = cli(root, &["review", "--strict"]);
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
        &case("001", "(sleep 1; printf escaped > escaped) &\nwait", ""),
    );
    let path = root.join("semspec.toml");
    let config = fs::read_to_string(&path).unwrap().replace("180s", "50ms");
    fs::write(path, config).unwrap();
    let (output, json) = run_json(root, &[]);
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
fn interpreter_launch_errors_are_not_expected_failures() {
    let dir = fixture();
    let root = dir.path();
    write_cases(
        root,
        &case(
            "001",
            "true",
            "<!-- semantic-case: xfail-on=all xfail-reason='known bug' -->",
        ),
    );
    let path = root.join("semspec.toml");
    let config = fs::read_to_string(&path)
        .unwrap()
        .replace("env = {}", "env = { PATH = '/no/interpreter/here' }");
    fs::write(path, config).unwrap();
    let (output, json) = run_json(root, &[]);
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(json["results"][0]["verdict"]["verdict"], "ERROR");
    clean_report(&json);
}

#[test]
fn duplicate_ids_and_retired_id_reuse_are_parse_errors() {
    let dir = fixture();
    let root = dir.path();
    let text = case("001", "true", "");
    write_cases(root, &(text.clone() + &text));
    assert_eq!(cli(root, &["lint"]).status.code(), Some(2));
    write_cases(root, &text);
    let path = root.join("semspec.toml");
    let config = fs::read_to_string(&path)
        .unwrap()
        .replace("retired = []", "retired = ['S-TEST-001']");
    fs::write(path, config).unwrap();
    assert_eq!(cli(root, &["lint"]).status.code(), Some(2));
}
