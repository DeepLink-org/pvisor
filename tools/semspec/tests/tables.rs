use serde_json::{Value, json};
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

fn write(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

#[test]
fn bench_supports_json_scalar_commands_and_rejects_invalid_templates_before_execution() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("in.json"),
        &json!({"native": {"value": "$ printf '12.5'", "note": "literal"}}),
    );
    assert!(cli(root, &["bench", "in.json"]).status.success());
    let rendered: Value =
        serde_json::from_slice(&fs::read(root.join("in.render.json")).unwrap()).unwrap();
    assert_eq!(rendered["native"]["value"], 12.5);
    write(
        &root.join("in.json"),
        &json!({"native": {"value": "$ touch executed; printf 1"}, "other": {"wrong": 1}}),
    );
    assert_eq!(cli(root, &["bench", "in.json"]).status.code(), Some(2));
    assert!(!root.join("executed").exists());
    assert!(root.join("in.render.json").exists());
    for command in [
        "$ printf not-json",
        "$ false",
        "$ printf '[1]'",
        "$ printf '\"$ unresolved\"'",
        "$ printf 1; false",
    ] {
        write(
            &root.join("in.json"),
            &json!({"native": {"value": command}}),
        );
        assert_eq!(
            cli(root, &["bench", "in.json"]).status.code(),
            Some(2),
            "{command}"
        );
        assert!(!root.join("in.render.json").exists());
    }
}

#[test]
fn render_is_read_only_resolves_relative_json_and_preserves_code_examples() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir(root.join("data")).unwrap();
    write(
        &root.join("data/in.render.json"),
        &json!({"Z|native": {"median ms": 2.13, "notes": "<tag>\na|b"}, "A": {"median ms": Value::Null, "notes": "plain"}}),
    );
    let source = "# Data\n\n{{ table(\"in.render.json\") }}\n\n```markdown\n{{ table(\"missing.json\") }}\n```\n\n    {{ table(\"missing.json\") }}\n\n`{{ table(\"missing.json\") }}`\n";
    fs::write(root.join("data/page.md"), source).unwrap();
    let output = cli(root, &["render", "data/page.md"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rendered = String::from_utf8(output.stdout).unwrap();
    assert!(
        rendered.contains("| Z&#124;native | 2.13 | &lt;tag&gt;<br>a&#124;b |"),
        "{rendered}"
    );
    assert!(rendered.contains("| A | — | plain |"));
    assert!(rendered.contains("```markdown\n{{ table(\"missing.json\") }}\n```"));
    assert!(rendered.contains("    {{ table(\"missing.json\") }}"));
    assert_eq!(
        fs::read_to_string(root.join("data/page.md")).unwrap(),
        source
    );
    assert_eq!(
        cli(
            root,
            &["render", "data/page.md", "--output", "data/page.md"]
        )
        .status
        .code(),
        Some(2)
    );
    assert!(
        cli(root, &["render", "data/page.md", "--output", "result.md"])
            .status
            .success()
    );
    assert_eq!(
        fs::read_to_string(root.join("result.md")).unwrap(),
        rendered
    );
}

#[test]
fn render_refuses_unresolved_commands_and_missing_or_malformed_tables() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::write(root.join("page.md"), "{{ table(\"in.json\") }}\n").unwrap();
    assert_eq!(cli(root, &["render", "page.md"]).status.code(), Some(2));
    write(
        &root.join("in.json"),
        &json!({"row": {"value": "$ touch executed; echo 1"}}),
    );
    assert_eq!(cli(root, &["render", "page.md"]).status.code(), Some(2));
    assert!(!root.join("executed").exists());
    for value in [
        json!({}),
        json!([]),
        json!({"row": {"x": [1]}}),
        json!({"row": {"x": 1}, "other": {"y": 2}}),
    ] {
        write(&root.join("in.json"), &value);
        assert_eq!(cli(root, &["render", "page.md"]).status.code(), Some(2));
    }
}

#[test]
fn live_render_keeps_valid_tables_and_recovers_after_a_missing_table_is_created() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("good.json"), &json!({"native": {"ms": 2}}));
    fs::write(root.join("bad.json"), "not json").unwrap();
    write(
        &root.join("command.json"),
        &json!({"row": {"x": "$ touch executed"}}),
    );
    let source = "# Preview\n\n{{ table(\"missing.json\") }}\n\n{{ table(\"good.json\") }}\n\n{{ table(\"bad.json\") }}\n\n{{ table(\"good.json\", {\"wrong\":true}) }}\n\n{{ table(\"command.json\") }}\n\n{{ table(invalid) }}\n\n```markdown\n{{ table(\"example.json\") }}\n```\n\nTail\n";
    fs::write(root.join("page.md"), source).unwrap();
    assert_eq!(cli(root, &["render", "page.md"]).status.code(), Some(2));
    let preview = cli(root, &["render", "page.md", "--best-effort"]);
    assert!(preview.status.success());
    let text = String::from_utf8(preview.stdout).unwrap();
    let errors = String::from_utf8(preview.stderr).unwrap();
    assert_eq!(text.matches("!!! warning").count(), 5);
    assert!(text.contains("| native | 2 |"));
    assert!(text.contains("```markdown\n{{ table(\"example.json\") }}\n```"));
    assert!(text.ends_with("Tail\n"));
    assert!(errors.contains("page.md:3:"));
    assert!(errors.contains("missing.json"));
    assert!(!root.join("executed").exists());
    assert_eq!(fs::read_to_string(root.join("page.md")).unwrap(), source);
    write(&root.join("missing.json"), &json!({"repaired": {"ms": 3}}));
    let preview = cli(root, &["render", "page.md", "--best-effort"]);
    let text = String::from_utf8(preview.stdout).unwrap();
    assert!(text.contains("| repaired | 3 |"));
    assert_eq!(text.matches("!!! warning").count(), 4);
}

#[test]
fn canonical_json_helper_keeps_sorted_nested_objects() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("data.json"), r#"{"z":0,"a":{"z":1,"a":2}}"#).unwrap();
    let output = cli(tmp.path(), &["helper", "json-get", "data.json", ""]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "{\"a\":{\"a\":2,\"z\":1},\"z\":0}\n"
    );
}

#[test]
fn render_options_preserve_trusted_article_markup_and_duplicate_labels() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("data.json"),
        &json!({"first": {"name": "[link](source.csv)", "value": "`a|b` **yes**<br>ok", "value2": "x\\|y\nnext"}}),
    );
    fs::write(root.join("page.md"), "{{ table(\"data.json\", {\"row_names\":false,\"markdown\":true,\"align\":[\"left\",\"right\",\"center\"],\"headers\":[\"Name\",\"Value\",\"Value\"]}) }}\n").unwrap();
    let result = cli(root, &["render", "page.md"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let text = String::from_utf8(result.stdout).unwrap();
    assert!(text.contains("| Name | Value | Value |\n| :--- | ---: | :---: |"));
    assert!(text.contains("| [link](source.csv) | `a\\|b` **yes**<br>ok | x\\|y<br>next |"));
    for options in [
        r#"{"row_names":false,"align":["left"]}"#,
        r#"{"headers":["one"]}"#,
        r#"{"markdown":"yes"}"#,
        r#"{"unknown":true}"#,
        r#"{"align":["invalid"]}"#,
    ] {
        fs::write(
            root.join("page.md"),
            format!("{{{{ table(\"data.json\", {options}) }}}}\n"),
        )
        .unwrap();
        assert_eq!(cli(root, &["render", "page.md"]).status.code(), Some(2));
    }
}

#[test]
fn script_expands_a_complete_row_once_and_preserves_result_and_provenance() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir(root.join("data")).unwrap();
    fs::create_dir(root.join("work")).unwrap();
    fs::write(root.join("work/fixture"), "ready").unwrap();
    // Stdout is the row; diagnostics go to stderr. Cwd and the hook log path
    // have the same contracts as existing scalar command cells.
    fs::write(root.join("work/measure.py"), "import json, os, sys\nfrom pathlib import Path\nassert Path('fixture').read_text() == 'ready'\nassert os.environ['SEMSPEC_BENCH_LOG']\nwith open('calls', 'a') as f: f.write('run\\n')\nprint('diagnostic', file=sys.stderr)\nprint(json.dumps({'N': 30, 'Median ms': 1.25, 'Mean ms': 1.5, 'SD ms': .1, 'Min ms': 1, 'Max ms': 2}))\n").unwrap();
    write(
        &root.join("data/in.json"),
        &json!({"native-read": "$ python3 measure.py", "same-script": "$ python3 measure.py", "literal": {"N": 30, "Median ms": 3, "Mean ms": 3, "SD ms": 0, "Min ms": 3, "Max ms": 3}}),
    );
    let result = cli(root, &["bench", "data/in.json", "--cwd", "work"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let table: Value =
        serde_json::from_slice(&fs::read(root.join("data/in.render.json")).unwrap()).unwrap();
    assert_eq!(table["native-read"]["Median ms"], 1.25);
    assert_eq!(table["same-script"], table["native-read"]);
    assert_eq!(table["literal"]["N"], 30);
    assert_eq!(
        table.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["native-read", "same-script", "literal"]
    );
    assert_eq!(
        table["native-read"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["N", "Median ms", "Mean ms", "SD ms", "Min ms", "Max ms"]
    );
    assert_eq!(
        fs::read_to_string(root.join("work/calls")).unwrap(),
        "run\n"
    );
    let evidence: Value =
        serde_json::from_slice(&fs::read(root.join("data/in.bench.json")).unwrap()).unwrap();
    assert_eq!(evidence["commands"].as_array().unwrap().len(), 1);
    assert_eq!(evidence["commands"][0]["kind"], "script-row");
    assert_eq!(evidence["commands"][0]["raw"], table["native-read"]);
    assert_eq!(evidence["rows"].as_array().unwrap().len(), 2);
    assert_eq!(evidence["rows"][0]["row"], "native-read");
    assert_eq!(evidence["rows"][0]["expression"], "python3 measure.py");
    let log = evidence["commands"][0]["log"].as_str().unwrap();
    assert!(fs::read_to_string(log).unwrap().contains("diagnostic"));
    fs::write(
        root.join("data/page.md"),
        "{{ table(\"in.render.json\") }}\n",
    )
    .unwrap();
    let result = cli(root, &["render", "data/page.md"]);
    assert!(result.status.success());
    assert!(
        String::from_utf8(result.stdout)
            .unwrap()
            .contains("| native-read | 30 | 1.25 | 1.5 | 0.1 | 1 | 2 |")
    );
}

#[test]
fn script_rows_and_cell_commands_can_share_a_table_but_not_return_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("in.json"),
        &json!({"script": "$ printf '%s' '{\"value\":2}'", "cell": {"value": "$ printf 3"}}),
    );
    assert!(cli(root, &["bench", "in.json"]).status.success());
    let table: Value =
        serde_json::from_slice(&fs::read(root.join("in.render.json")).unwrap()).unwrap();
    assert_eq!(table, json!({"script": {"value": 2}, "cell": {"value": 3}}));
    for output in [
        "null",
        "[]",
        "7",
        "{}",
        "{\"value\":[]}",
        "{\"value\":{}}",
        "{\"value\":\"$ touch executed\"}",
        "{\"wrong\":2}",
        "not-json",
        "{} {}",
    ] {
        write(
            &root.join("in.json"),
            &json!({"script": format!("$ printf '%s' '{output}'"), "cell": {"value": 3}}),
        );
        fs::write(root.join("in.render.json"), "stale").unwrap();
        fs::write(root.join("in.bench.json"), "stale").unwrap();
        assert_eq!(
            cli(root, &["bench", "in.json"]).status.code(),
            Some(2),
            "{output}"
        );
        assert!(!root.join("in.render.json").exists());
        assert!(!root.join("in.bench.json").exists());
        assert!(!root.join("executed").exists());
        assert!(root.join("in.bench.logs/0001.log").exists());
    }
    write(
        &root.join("in.json"),
        &json!({"script": "$ printf '{\"value\":1}'; exit 9"}),
    );
    assert_eq!(cli(root, &["bench", "in.json"]).status.code(), Some(2));
    // Rendering a template must reject the unresolved row without running it.
    write(
        &root.join("in.json"),
        &json!({"script": "$ touch executed; printf '{\"value\":1}'"}),
    );
    fs::write(root.join("page.md"), "{{ table(\"in.json\") }}\n").unwrap();
    assert_eq!(cli(root, &["render", "page.md"]).status.code(), Some(2));
    assert!(!root.join("executed").exists());
}

#[test]
fn invalid_static_columns_and_row_command_plans_preserve_previous_results() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for table in [
        json!({"script": "$ touch executed; printf '{}'", "a": {"value": 1}, "b": {"wrong": 2}}),
        json!({"script": "$ printf '{\"value\":1}'", "cell": {"value": "$ printf '{\"value\":1}'"}}),
        json!({"script": "$ 'unmatched"}),
    ] {
        write(&root.join("in.json"), &table);
        fs::write(root.join("in.render.json"), "previous result").unwrap();
        fs::write(root.join("in.bench.json"), "previous evidence").unwrap();
        assert_eq!(cli(root, &["bench", "in.json"]).status.code(), Some(2));
        assert_eq!(
            fs::read_to_string(root.join("in.render.json")).unwrap(),
            "previous result"
        );
        assert_eq!(
            fs::read_to_string(root.join("in.bench.json")).unwrap(),
            "previous evidence"
        );
        assert!(!root.join("executed").exists());
    }
}

#[test]
fn script_row_preserves_full_float_precision() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let input = root.join("in.json");
    fs::write(
        &input,
        r#"{"VM":"$ printf '%s' '{\"mean\":196.85512733333334}'"}"#,
    )
    .unwrap();
    let status = cli(root, &["bench", "in.json"]);
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let text = fs::read_to_string(root.join("in.render.json")).unwrap();
    assert!(text.contains("196.85512733333334"), "{text}");
}
