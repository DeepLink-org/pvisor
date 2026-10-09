use semspec::{
    helpers,
    ledger::{Approval, Ledger},
    model::{CaseResult, ReviewState, RunReport, Verdict},
    parse::{lint_bash, parse_document},
    seal,
};
use std::{fs, path::Path};

#[test]
fn stable_digests_bind_normalized_text_vocabulary_and_engine() {
    assert_eq!(seal::normalize("α \r\nβ\t\n\n"), "α\nβ\n");
    assert_ne!(seal::normalize("é"), seal::normalize("e\u{301}"));
    assert_eq!(
        seal::engine_digest(),
        "sha256:67dcdab88d011a8c32fc87238c86746a90b5f08d75b9ef8be217c2baadb516c1"
    );
    assert_eq!(
        seal::vocab_digest("core.sh", "echo ok \n\n"),
        "sha256:d20612e5882b58b1fa423b007ee83f906f21e5b9af92cf743e4070b913a51191"
    );
    let digest = seal::case_digest("语义 \n\n", &["z".into(), "a".into()]);
    assert_eq!(
        digest,
        "sha256:02542aa200b49050121aec2a48e58d045e98f6cfe1e8d3c54b16ad5bb2d1a4b9"
    );
    assert_eq!(
        digest,
        seal::case_digest("语义\n", &["a".into(), "z".into()])
    );
    assert_ne!(
        digest,
        seal::case_digest("语义改变\n", &["a".into(), "z".into()])
    );
    assert_ne!(
        digest,
        seal::case_digest("语义\n", &["a".into(), "x".into()])
    );
}

pub fn spec(id: &str, script: &str, annotation: &str) -> String {
    format!(
        "### {id}：测试\n\n**语义**：行为必须保持。\n\n**违反示例**：改变结果。\n\n<!-- semspec: case id={id} {annotation} -->\n```bash\n{script}\n```\n"
    )
}

#[test]
fn markdown_boundaries_and_bash_scope_are_structural() {
    let text = spec(
        "S-TEST-001",
        "printf '%s\\n' '### S-FAKE-002: not a heading'\n# source not-code",
        "xfail-on=all xfail-reason='known issue'",
    );
    let cases = parse_document(Path::new("test.md"), &text).unwrap();
    assert_eq!(cases.len(), 1);
    assert_eq!(
        cases[0].annotation.xfail_reason.as_deref(),
        Some("known issue")
    );
    assert!(parse_document(Path::new("test.md"), &text.replace("行为必须保持。", "")).is_err());
    assert!(
        parse_document(
            Path::new("test.md"),
            &text.replace("xfail-on=all", "unknown=all")
        )
        .is_err()
    );
    assert!(
        parse_document(
            Path::new("test.md"),
            text.trim_end().trim_end_matches("```")
        )
        .is_err()
    );
    for code in [
        "source other.sh",
        ". other.sh",
        "builtin source other.sh",
        "SEMSPEC_BIN=x",
    ] {
        lint_bash(code).unwrap();
    }
    assert!(lint_bash("if then").is_err());
}

#[test]
fn ledger_states_and_result_exit_codes_are_independent() {
    let mut ledger = Ledger::default();
    let digest = seal::engine_digest();
    assert_eq!(ledger.state("@engine", &digest), ReviewState::Unreviewed);
    let approval = Approval {
        item: "@engine".into(),
        digest: digest.clone(),
        reviewer: "test human".into(),
        date: "2026-10-01".into(),
        signature: None,
    };
    ledger.approve(approval.clone());
    let ledger_text = toml::to_string(&ledger).unwrap();
    ledger = Ledger::parse(&ledger_text).unwrap();
    assert!(ledger.state("@engine", &digest).reviewed());
    assert!(matches!(
        ledger.state("@engine", "changed"),
        ReviewState::Stale { .. }
    ));
    let mut report = RunReport {
        engine_semantics: "1",
        engine_review: ReviewState::Unreviewed,
        vocab_review: vec![],
        platform: "test".into(),
        results: vec![],
    };
    for (verdict, code) in [
        (Verdict::Pass, 0),
        (
            Verdict::Skip {
                reason: "absent".into(),
            },
            0,
        ),
        (
            Verdict::XFail {
                reason: "issue".into(),
                output_tail: "failed".into(),
            },
            0,
        ),
        (Verdict::XPass, 1),
        (
            Verdict::Fail {
                output_tail: "failed".into(),
            },
            1,
        ),
        (
            Verdict::Error {
                message: "internal".into(),
            },
            3,
        ),
    ] {
        report.results = vec![CaseResult {
            id: "S-TEST-001".into(),
            verdict,
            review: ReviewState::Unreviewed,
            duration_ms: 0,
            workdir: None,
        }];
        assert_eq!(report.exit_code(&["S-TEST-001"], false, false), code);
        assert_eq!(
            report.exit_code(&["S-TEST-001"], false, true),
            if code == 3 {
                3
            } else if matches!(report.results[0].verdict, Verdict::Pass) {
                0
            } else {
                1
            }
        );
        assert_eq!(
            report.exit_code(&["S-TEST-001"], true, false),
            if code == 3 { 3 } else { 1 }
        );
    }
}

#[test]
fn tree_state_tracks_full_bytes_modes_and_dangling_links() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file");
    fs::write(&path, b"a\n").unwrap();
    let before = helpers::tree_state(dir.path()).unwrap();
    fs::write(&path, b"a").unwrap();
    assert_ne!(before, helpers::tree_state(dir.path()).unwrap());
    let before = helpers::tree_state(dir.path()).unwrap();
    let mode = fs::metadata(dir.path()).unwrap().permissions().mode();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(mode ^ 0o010)).unwrap();
    assert_ne!(before, helpers::tree_state(dir.path()).unwrap());
    let before = helpers::tree_state(dir.path()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    assert_ne!(before, helpers::tree_state(dir.path()).unwrap());
    symlink("/definitely/absent", dir.path().join("link")).unwrap();
    assert!(
        helpers::tree_state(dir.path())
            .unwrap()
            .contains("link \"link\" -> \"/definitely/absent\"")
    );
    let json = dir.path().join("data.json");
    fs::write(&json, r#"{"a/b":{"~key":"hello\n"}}"#).unwrap();
    assert_eq!(
        helpers::json_get(&json, "/a~1b/~0key").unwrap(),
        "\"hello\\n\""
    );
    assert!(helpers::json_get(&json, "/a~2b").is_err());
}

#[test]
fn annotated_fences_define_cases_independently_of_headings() {
    use semspec::parse::{parse_document, parse_setup};
    let source = "# Ordinary tutorial\n\nExpected behavior described in prose.\n\n```bash\nexit 9\n```\n\n<!-- semspec: setup -->\n```bash\nvalue=ready\n```\n\n<!-- semspec: case id=S-TEST-001 timeout=50ms xfail-on=all xfail-reason='known issue' -->\n```bash\n[ \"$value\" = ready ]\n```\n\n<!-- semspec: case id=S-TEST-002 -->\n```bash\ntrue\n```\n\n### S-FAKE-003: an ordinary heading\n\n````markdown\n<!-- semspec: case id=S-FAKE-004 -->\n```bash\nfalse\n```\n````\n";
    let cases = parse_document(Path::new("tutorial.md"), source).unwrap();
    assert_eq!(cases.len(), 2);
    assert_eq!(cases[0].id, "S-TEST-001");
    assert_eq!(cases[0].title, "Ordinary tutorial");
    assert_eq!(cases[0].annotation.timeout.as_deref(), Some("50ms"));
    assert_eq!(
        cases[0].annotation.xfail_reason.as_deref(),
        Some("known issue")
    );
    assert_eq!(cases[0].script, "[ \"$value\" = ready ]\n");
    assert_eq!(parse_setup(source).unwrap(), "value=ready\n");
    for comment in [
        "<!-- semspec: case -->",
        "<!-- semspec: case id=bad -->",
        "<!-- semspec: case id=S-TEST-001 id=S-TEST-002 -->",
        "<!-- semspec: case id=S-TEST-001 unknown=value -->",
        "<!-- semspec: case id=S-TEST-001 timeout=0ms -->",
        "<!-- semspec: case id=S-TEST-001 xfail-on=all -->",
        "<!-- semspec: case id=S-TEST-001 -->\nintervening prose\n",
        "<!-- semspec: case id=S-TEST-001\ntimeout=1s -->",
    ] {
        let text = format!("{comment}\n```bash\ntrue\n```\n");
        assert!(
            parse_document(Path::new("test.md"), &text).is_err(),
            "{comment}"
        );
    }
    for text in [
        "<!-- semspec: case id=S-TEST-001 -->",
        "<!-- semspec: setup -->\n```python\npass\n```",
        "<!-- semspec: case id=S-TEST-001 -->\n```bash\ntrue\n",
    ] {
        assert!(parse_document(Path::new("test.md"), text).is_err());
    }
}

#[test]
fn migration_preserves_existing_claims_scripts_and_xfail() {
    use std::collections::BTreeMap;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let expected: BTreeMap<String, String> =
        serde_json::from_str(include_str!("migration_contracts.json")).unwrap();
    let mut actual = BTreeMap::new();
    for input in ["docs/src/zh/cases", "docs/src/zh/reference"] {
        let project = semspec::project::Project::load(&[root.join(input)]).unwrap();
        for case in project
            .cases
            .into_iter()
            .filter(|case| case.domain != "USE")
        {
            let fields = [
                case.id.clone(),
                case.claim,
                case.violation,
                seal::normalize(&case.script),
                case.annotation
                    .xfail_on
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(","),
                case.annotation.xfail_reason.unwrap_or_default(),
            ];
            actual.insert(case.id, seal::digest(&[fields.join("\0").as_bytes()]));
        }
    }
    assert_eq!(actual.len(), 74);
    assert_eq!(actual, expected);
}

#[test]
fn report_gate_requires_exact_nonempty_inventory_without_approving_cases() {
    let row = |id: &str| CaseResult {
        id: id.into(),
        verdict: Verdict::Pass,
        review: ReviewState::Unreviewed,
        duration_ms: 0,
        workdir: None,
    };
    let mut report = RunReport {
        engine_semantics: semspec::seal::ENGINE_SEMANTICS,
        engine_review: ReviewState::Unreviewed,
        vocab_review: vec![],
        platform: "test".into(),
        results: vec![],
    };
    let expected = ["S-TEST-001", "S-TEST-002"];
    for ids in [
        vec![],
        vec!["S-TEST-001"],
        vec!["S-TEST-001", "S-TEST-002", "S-TEST-003"],
        vec!["S-TEST-001", "S-TEST-001"],
    ] {
        report.results = ids.into_iter().map(row).collect();
        assert_eq!(report.exit_code(&expected, false, false), 1);
        assert_eq!(report.exit_code(&expected, false, true), 1);
    }
    report.results = expected.into_iter().map(row).collect();
    assert_eq!(report.exit_code(&expected, false, true), 0);
    assert_eq!(report.exit_code(&expected, true, true), 1);
    assert_eq!(report.results[0].review, ReviewState::Unreviewed);
}
