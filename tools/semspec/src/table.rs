//! Executable JSON tables and read-only Markdown substitution. These commands
//! are independent of semantic cases, verdicts, review ledgers and digests.
use anyhow::{Context, Result, ensure};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use crate::project::atomic_write;

/// Execute trusted Bash command rows/cells once per distinct expression.
/// Rows must print a nonempty JSON object of scalar columns; cells print a scalar.
/// Results and raw JSON evidence are published only after every command succeeds.
pub fn bench(input: &Path) -> Result<PathBuf> {
    bench_at(input, None)
}

/// Execute a JSON table using an optional explicit working directory.
/// Commands receive SEMSPEC_BENCH_LOG; stdout and stderr are retained per command.
pub fn bench_at(input: &Path, cwd: Option<&Path>) -> Result<PathBuf> {
    let input = input.canonicalize().context("read benchmark template")?;
    ensure!(
        input.extension().is_some_and(|ext| ext == "json"),
        "bench input must be a .json file"
    );
    let output = input.with_extension("render.json");
    let evidence = input.with_extension("bench.json");
    let mut table: Value = serde_json::from_slice(&fs::read(&input)?)?;
    validate_table(&table, true)?;
    let cwd = cwd
        .unwrap_or(input.parent().unwrap())
        .canonicalize()
        .context("command cwd must exist")?;
    ensure!(cwd.is_dir(), "command cwd must be a directory");
    let mut shapes = HashMap::new();
    for row in table.as_object().unwrap().values() {
        let values = row
            .as_object()
            .map(|row| row.values().collect::<Vec<_>>())
            .unwrap_or_else(|| vec![row]);
        for cell in values {
            if let Some(command) = cell.as_str().and_then(|s| s.strip_prefix("$ ")) {
                ensure!(!command.trim().is_empty(), "empty command cell");
                shell_words::split(command).context("invalid command quoting")?;
                if let Some(previous) = shapes.insert(command.to_owned(), !row.is_object()) {
                    ensure!(
                        previous == !row.is_object(),
                        "one script command cannot produce both a row object and a scalar cell"
                    );
                }
            }
        }
    }
    for path in [&output, &evidence] {
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    let log_dir = input.with_extension("bench.logs");
    fs::create_dir_all(&log_dir)?;
    let mut cache = HashMap::<String, Value>::new();
    let mut records = Vec::new();
    let mut cells = Vec::new();
    let mut rows = Vec::new();
    for (row_name, row) in table.as_object_mut().unwrap() {
        if let Some(command) = row
            .as_str()
            .and_then(|s| s.strip_prefix("$ "))
            .map(str::to_owned)
        {
            let raw = cached_execute(
                &command,
                &cwd,
                &log_dir,
                "script-row",
                &mut cache,
                &mut records,
            )
            .with_context(|| format!("benchmark row {row_name:?}"))?;
            validate_table(&json!({row_name: raw}), false)
                .with_context(|| format!("script output for row {row_name:?}"))?;
            rows.push(json!({"row": row_name, "command": command, "expression": command,
                "columns": raw.as_object().context("row command must print a JSON object")?.keys().collect::<Vec<_>>()}));
            *row = raw;
        } else {
            for (column, cell) in row.as_object_mut().unwrap() {
                let Some(command) = cell
                    .as_str()
                    .and_then(|s| s.strip_prefix("$ "))
                    .map(str::to_owned)
                else {
                    continue;
                };
                let raw = cached_execute(
                    &command,
                    &cwd,
                    &log_dir,
                    "script-cell",
                    &mut cache,
                    &mut records,
                )
                .with_context(|| format!("benchmark cell {row_name:?} / {column:?}"))?;
                ensure!(
                    is_scalar(&raw) && !raw.as_str().is_some_and(|s| s.starts_with("$ ")),
                    "cell command must produce a resolved JSON scalar"
                );
                *cell = raw;
                cells.push(json!({"row": row_name, "column": column, "command": command, "expression": command}));
            }
        }
    }
    validate_table(&table, false)?;
    atomic_write(
        &evidence,
        &json_bytes(
            &json!({"format":1,"template":input,"cwd":cwd,"commands":records,"cells":cells,"rows":rows}),
        )?,
    )?;
    atomic_write(&output, &json_bytes(&table)?)?;
    Ok(output)
}

fn cached_execute(
    command: &str,
    cwd: &Path,
    log_dir: &Path,
    kind: &str,
    cache: &mut HashMap<String, Value>,
    records: &mut Vec<Value>,
) -> Result<Value> {
    if let Some(raw) = cache.get(command) {
        return Ok(raw.clone());
    }
    let log = log_dir.join(format!("{:04}.log", records.len() + 1));
    eprintln!("bench: {command}");
    let output = Command::new("bash")
        .args(["-euo", "pipefail", "-c", command])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env("SEMSPEC_BENCH_LOG", &log)
        .output()
        .context("launch command")?;
    fs::write(
        &log,
        [output.stdout.as_slice(), output.stderr.as_slice()].concat(),
    )?;
    ensure!(
        output.status.success(),
        "command exited {}; see {}",
        output.status,
        log.display()
    );
    let raw: Value =
        serde_json::from_slice(&output.stdout).context("command stdout must be one JSON value")?;
    records.push(json!({"command":command,"kind":kind,"raw":raw,"value":raw,"log":log}));
    cache.insert(command.to_owned(), raw.clone());
    Ok(raw)
}

fn is_scalar(value: &Value) -> bool {
    !value.is_object() && !value.is_array()
}

fn validate_table(table: &Value, commands: bool) -> Result<()> {
    let rows = table
        .as_object()
        .context("table must be an object of named rows")?;
    ensure!(!rows.is_empty(), "table must have at least one row");
    let columns = rows.values().find_map(Value::as_object);
    for (name, row) in rows {
        if row.as_str().is_some_and(|s| s.starts_with("$ ")) {
            ensure!(
                commands,
                "unresolved row command at {name:?}; run semspec bench first"
            );
            continue;
        }
        let row = row
            .as_object()
            .with_context(|| format!("row {name:?} must be an object"))?;
        ensure!(!row.is_empty(), "table must have at least one column");
        let columns = columns.unwrap();
        ensure!(
            row.len() == columns.len() && columns.keys().all(|key| row.contains_key(key)),
            "row {name:?} has different columns"
        );
        for (column, value) in row {
            ensure!(
                is_scalar(value),
                "cell {name:?} / {column:?} must be a scalar"
            );
            ensure!(
                commands || !value.as_str().is_some_and(|s| s.starts_with("$ ")),
                "unresolved command at {name:?} / {column:?}; run semspec bench first"
            );
        }
    }
    Ok(())
}

fn json_bytes(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Substitute standalone `{{ table("relative.json") }}` lines with Markdown
/// tables. JSON paths are relative to `input`; code blocks and inline examples
/// remain literal. This function only reads files and never executes commands.
/// Rows and columns retain JSON insertion order. In strict mode, table errors
/// fail the render. With `best_effort`, each failed placeholder becomes a visible
/// warning, diagnostics go to stderr and other tables continue to render.
pub fn render(source: &str, input: &Path, best_effort: bool) -> Result<String> {
    let mut protected = Vec::new();
    let mut start = None;
    for (event, range) in Parser::new(source).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => start = Some(range.start),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = start.take() {
                    protected.push(start..range.end);
                }
            }
            Event::Code(_) => protected.push(range),
            _ => {}
        }
    }
    let mut text = String::new();
    let mut offset = 0;
    for (index, line) in source.split_inclusive('\n').enumerate() {
        let trimmed = line.trim();
        if !protected
            .iter()
            .any(|r| r.start < offset + line.len() && offset < r.end)
            && trimmed.starts_with("{{ table(")
        {
            let rendered = match render_placeholder(trimmed, input) {
                Ok(rendered) => rendered,
                Err(error) if best_effort => {
                    eprintln!(
                        "semspec: warning: {}:{}: {error:#}",
                        input.display(),
                        index + 1
                    );
                    format!(
                        "!!! warning \"Table unavailable during live preview\"\n\n    {}",
                        escape(&format!("{error:#}"))
                    )
                }
                Err(error) => return Err(error),
            };
            // Blank lines separate a table from adjacent paragraphs.
            if !text.is_empty() && !text.ends_with("\n\n") {
                text.push('\n');
            }
            let indent = &line[..line.len() - line.trim_start().len()];
            for line in rendered.lines() {
                text.push_str(indent);
                text.push_str(line);
                text.push('\n');
            }
            text.push('\n');
        } else {
            text.push_str(line);
        }
        offset += line.len();
    }
    Ok(text)
}

fn render_placeholder(placeholder: &str, input: &Path) -> Result<String> {
    let path = placeholder
        .strip_prefix("{{ table(")
        .unwrap()
        .strip_suffix(") }}")
        .context("table placeholder must be {{ table(\"file.json\") }} on its own line")?;
    let args: Vec<Value> = serde_json::from_str(&format!("[{path}]"))
        .context("expected a JSON filename and optional table options")?;
    ensure!(
        matches!(args.len(), 1 | 2),
        "expected filename and optional table options"
    );
    let path = args[0]
        .as_str()
        .context("table path must be a JSON string")?;
    let options: TableOptions = if args.len() == 2 {
        serde_json::from_value(args[1].clone()).context("invalid table options")?
    } else {
        TableOptions::default()
    };
    let file = input.parent().unwrap_or(Path::new(".")).join(path);
    let table: Value = serde_json::from_slice(
        &fs::read(&file).with_context(|| format!("read table {}", file.display()))?,
    )?;
    validate_table(&table, false).with_context(|| format!("table {}", file.display()))?;
    markdown_table(table.as_object().unwrap(), &options)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\\', "&#92;")
        .replace('|', "&#124;")
        .replace('`', "&#96;")
        .replace('*', "&#42;")
        .replace('_', "&#95;")
        .replace('[', "&#91;")
        .replace(']', "&#93;")
        .replace("\r\n", "\n")
        .replace(['\n', '\r'], "<br>")
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TableOptions {
    row_names: bool,
    markdown: bool,
    align: Option<Vec<Alignment>>,
    headers: Option<Vec<String>>,
}

impl Default for TableOptions {
    fn default() -> Self {
        Self {
            row_names: true,
            markdown: false,
            align: None,
            headers: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Alignment {
    Left,
    Right,
    Center,
    Default,
}

fn table_text(text: &str, markdown: bool) -> String {
    if !markdown {
        return escape(text);
    }
    // Trusted article markup is opt-in. Escape table delimiters even inside
    // code spans; preserve existing escaped pipes and all other Markdown.
    let mut output = String::new();
    let mut slashes = 0;
    for c in text.replace("\r\n", "\n").chars() {
        if c == '|' && slashes % 2 == 0 {
            output.push('\\');
        }
        match c {
            '\n' | '\r' => output.push_str("<br>"),
            _ => output.push(c),
        }
        slashes = if c == '\\' { slashes + 1 } else { 0 };
    }
    output
}

fn markdown_table(rows: &Map<String, Value>, options: &TableOptions) -> Result<String> {
    let columns = rows.values().next().unwrap().as_object().unwrap();
    let count = columns.len() + usize::from(options.row_names);
    if let Some(align) = &options.align {
        ensure!(
            align.len() == count,
            "alignment count must match rendered columns"
        );
    }
    let mut headers = Vec::new();
    if options.row_names {
        headers.push(String::new());
    }
    headers.extend(columns.keys().map(|key| table_text(key, options.markdown)));
    if let Some(labels) = &options.headers {
        ensure!(
            labels.len() == count,
            "header count must match rendered columns"
        );
        headers = labels
            .iter()
            .map(|label| table_text(label, options.markdown))
            .collect();
    }
    let separators: Vec<_> = (0..count)
        .map(|i| match options.align.as_ref().map(|a| &a[i]) {
            Some(Alignment::Left) => ":---",
            Some(Alignment::Right) => "---:",
            Some(Alignment::Center) => ":---:",
            _ => "---",
        })
        .collect();
    let mut text = format!(
        "| {} |\n| {} |\n",
        headers.join(" | "),
        separators.join(" | ")
    );
    for (name, row) in rows {
        let mut cells = Vec::new();
        if options.row_names {
            cells.push(table_text(name, options.markdown));
        }
        cells.extend(columns.keys().map(|key| match &row[key] {
            Value::String(s) => table_text(s, options.markdown),
            Value::Null => "—".into(),
            value => value.to_string(),
        }));
        text.push_str(&format!("| {} |\n", cells.join(" | ")));
    }
    Ok(text)
}
