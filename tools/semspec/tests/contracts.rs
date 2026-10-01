use semspec::{
    helpers,
    ledger::{Approval, Ledger, Revocation},
    model::{CaseResult, ReviewState, RunReport, Verdict},
    parse::{lint_bash, parse_spec},
    seal,
};
use std::{fs, path::Path};

#[test]
fn stable_digests_bind_normalized_text_vocabulary_and_engine() {
    assert_eq!(seal::normalize("α \r\nβ\t\n\n"), "α\nβ\n");
    assert_ne!(seal::normalize("é"), seal::normalize("e\u{301}"));
    assert_eq!(
        seal::engine_digest(),
        "sha256:eb25064cae571ccbff7e8a348d7d5b2bfb32e46912743af70087ee1568d59440"
    );
    assert_eq!(
        seal::vocab_digest("core.sh", "echo ok \n\n"),
        "sha256:d20612e5882b58b1fa423b007ee83f906f21e5b9af92cf743e4070b913a51191"
    );
    let digest = seal::case_digest("语义 \n\n", &["z".into(), "a".into()]);
    assert_eq!(
        digest,
        "sha256:2e1b62932a03928971bf211767b1731de3c7090ba02adf17bfea788dbf4413b7"
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
        "### {id}：测试\n\n**语义**：行为必须保持。\n\n**违反示例**：改变结果。\n\n{annotation}\n\n```bash\n{script}\n```\n"
    )
}

#[test]
fn markdown_boundaries_and_bash_scope_are_structural() {
    let text = spec(
        "S-TEST-001",
        "printf '%s\\n' '### S-FAKE-002: not a heading'\n# source not-code",
        "<!-- semantic-case: xfail-on=all xfail-reason='known issue' -->",
    );
    let cases = parse_spec(Path::new("test.md"), &text).unwrap();
    assert_eq!(cases.len(), 1);
    assert_eq!(
        cases[0].annotation.xfail_reason.as_deref(),
        Some("known issue")
    );
    assert!(parse_spec(Path::new("test.md"), &text.replace("**语义**", "**遗漏**")).is_err());
    assert!(
        parse_spec(
            Path::new("test.md"),
            &text.replace("xfail-on=all", "unknown=all")
        )
        .is_err()
    );
    assert!(
        parse_spec(
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
        "export SEMSPEC_BIN=x",
        "echo $(source other.sh)",
    ] {
        assert!(lint_bash(code).is_err(), "accepted {code}");
    }
    for code in [
        "echo 'source other.sh'",
        "# . other.sh\necho ok",
        "cat <<'END'\nsource other.sh\nEND\n",
    ] {
        lint_bash(code).unwrap();
    }
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
    ledger.revoke(Revocation {
        item: "@engine".into(),
        reviewer: "test human".into(),
        date: "2026-10-02".into(),
        reason: "review again".into(),
    });
    assert!(matches!(
        ledger.state("@engine", &digest),
        ReviewState::Revoked { .. }
    ));
    ledger.approve(approval);
    assert!(ledger.state("@engine", &digest).reviewed());
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
                requirement: "fuse".into(),
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
        assert_eq!(report.exit_code(false), code);
        assert_eq!(report.exit_code(true), if code == 3 { 3 } else { 1 });
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
