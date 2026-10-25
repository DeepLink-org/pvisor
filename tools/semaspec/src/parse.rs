use crate::{
    model::{Annotation, Case, Expectation, valid_case_id},
    seal::normalize,
};
use anyhow::{Context, Result, ensure};
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
use std::{collections::BTreeMap, path::Path, time::Duration};

/// Extract explicitly executable preparation blocks; ordinary tutorial fences are prose.
pub fn parse_setup(source: &str) -> Result<String> {
    let mut script = String::new();
    for block in marked_blocks(source)? {
        if block.parameters == "setup" {
            script.push_str(&block.script);
        }
    }
    lint_bash(&script)?;
    Ok(script)
}

pub(crate) struct MarkedBlock {
    pub start: usize,
    pub end: usize,
    pub parameters: String,
    pub script: String,
}
pub(crate) fn marked_blocks(source: &str) -> Result<Vec<MarkedBlock>> {
    let mut pending = None;
    let mut blocks = vec![];
    for (event, range) in Parser::new(source).into_offset_iter() {
        match event {
            Event::Html(html) | Event::InlineHtml(html)
                if html.trim().starts_with("<!-- semspec:") =>
            {
                ensure!(
                    pending.is_none(),
                    "semaspec comment must immediately precede a code block"
                );
                ensure!(
                    html.trim().lines().count() == 1,
                    "semaspec comment must be one line"
                );
                ensure!(
                    source[..range.start]
                        .rsplit('\n')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .is_empty(),
                    "semaspec comment must occupy its own line"
                );
                let parameters = html
                    .trim()
                    .strip_prefix("<!-- semspec:")
                    .and_then(|s| s.strip_suffix("-->"))
                    .context("invalid semaspec comment")?
                    .trim()
                    .to_owned();
                ensure!(
                    parameters == "setup"
                        || ["case ", "expect ", "bench ", "table "]
                            .iter()
                            .any(|p| parameters.starts_with(p)),
                    "expected semspec: case, setup, expect, bench or table"
                );
                pending = Some((range, parameters));
            }
            Event::Start(Tag::CodeBlock(kind)) if pending.is_some() => {
                let (comment, parameters) = pending.take().unwrap();
                ensure!(
                    source[comment.end..range.start].trim().is_empty(),
                    "semaspec comment must immediately precede a code block"
                );
                let language = match kind {
                    CodeBlockKind::Fenced(lang) => lang.into_string(),
                    _ => anyhow::bail!("marked cells require a fenced block"),
                };
                let expected = if parameters.starts_with("expect ") {
                    "text"
                } else if parameters.starts_with("bench ") {
                    "toml"
                } else if parameters.starts_with("table ") {
                    "json"
                } else {
                    "bash"
                };
                ensure!(
                    language == expected,
                    "marked cell requires a {expected} fence"
                );
                let script = fenced_body(&source[range.clone()])?;
                if expected == "bash" {
                    lint_bash(&script)?;
                }
                blocks.push(MarkedBlock {
                    start: comment.start,
                    end: range.end,
                    parameters,
                    script,
                });
            }
            _ => {}
        }
    }
    ensure!(
        pending.is_none(),
        "semaspec comment has no following code block"
    );
    Ok(blocks)
}

/// Cases are annotated fences; headings and unmarked examples never declare tests.
pub fn parse_document(path: &Path, source: &str) -> Result<Vec<Case>> {
    let mut headings = vec![];
    let mut heading = None;
    for (event, range) in Parser::new(source).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some((range.start, level, String::new()))
            }
            Event::Text(text) | Event::Code(text) if heading.is_some() => {
                heading.as_mut().unwrap().2.push_str(&text)
            }
            Event::End(TagEnd::Heading(_)) => headings.push(heading.take().unwrap()),
            _ => {}
        }
    }
    let mut cases = vec![];
    let blocks = marked_blocks(source)?;
    for block in &blocks {
        if !block.parameters.starts_with("case ") {
            continue;
        }
        let mut id = None;
        let mut metadata = vec![];
        for word in shell_words::split(block.parameters.strip_prefix("case ").unwrap())? {
            let (key, value) = word
                .split_once('=')
                .context("case parameters must be key=value")?;
            if key == "id" {
                ensure!(
                    valid_case_id(value) && id.is_none(),
                    "missing/duplicate/invalid case id"
                );
                id = Some(value.to_owned());
            } else {
                ensure!(
                    key != "vocab",
                    "document cases use inline preparation, not vocab files"
                );
                metadata.push(shell_words::quote(&word).into_owned());
            }
        }
        let id = id.context("case comment requires id=S-DOMAIN-NNN")?;
        let annotation = parse_annotation(&metadata.join(" "))?;
        let preceding = headings
            .iter()
            .rposition(|(start, _, _)| *start < block.start);
        let (start, end, title) = if let Some(i) = preceding {
            let (start, level, title) = &headings[i];
            let end = headings[i + 1..]
                .iter()
                .find(|(_, l, _)| *l <= *level)
                .map(|(offset, _, _)| *offset)
                .unwrap_or(source.len());
            (*start, end, title.clone())
        } else {
            (0, source.len(), id.clone())
        };
        let text = normalize(&source[start..end]);
        let mut prose = BTreeMap::new();
        for (event, range) in Parser::new(&text).into_offset_iter() {
            if let Event::Start(Tag::Paragraph) = event {
                for label in ["语义", "违反示例", "理由"] {
                    if let Some(value) = text[range.clone()].strip_prefix(&format!("**{label}**")) {
                        let value = value
                            .trim_start()
                            .strip_prefix([':', '：'])
                            .context("prose label needs colon")?
                            .trim();
                        ensure!(
                            !value.is_empty() && prose.insert(label, value.to_owned()).is_none(),
                            "empty/duplicate {label}"
                        );
                    }
                }
            }
        }
        cases.push(Case {
            domain: id.split('-').nth(1).unwrap().into(),
            id,
            title,
            file: path.into(),
            start_line: source[..block.start]
                .bytes()
                .filter(|c| *c == b'\n')
                .count()
                + 1,
            end_line: source[..block.end].lines().count(),
            claim: prose.remove("语义").unwrap_or_else(|| text.clone()),
            violation: prose.remove("违反示例").unwrap_or_default(),
            rationale: prose.remove("理由"),
            text,
            script: block.script.clone(),
            annotation,
            preparation: vec![],
            expectations: vec![],
        });
    }
    for block in blocks
        .iter()
        .filter(|b| b.parameters.starts_with("expect "))
    {
        let mut fields = BTreeMap::new();
        for word in shell_words::split(block.parameters.strip_prefix("expect ").unwrap())? {
            let (key, value) = word
                .split_once('=')
                .context("expect parameters must be key=value")?;
            ensure!(
                ["case", "stream"].contains(&key)
                    && !value.is_empty()
                    && fields.insert(key.to_owned(), value.to_owned()).is_none(),
                "invalid/duplicate expect parameter {key}"
            );
        }
        let id = fields
            .get("case")
            .context("expect requires case=S-DOMAIN-NNN")?;
        let stream = fields
            .get("stream")
            .context("expect requires stream=stdout or stderr")?;
        ensure!(
            ["stdout", "stderr"].contains(&stream.as_str()),
            "invalid expected stream"
        );
        let case = cases
            .iter_mut()
            .find(|c| c.id == *id)
            .context("expect references unknown case in this document")?;
        ensure!(
            !case.expectations.iter().any(|e| e.stream == *stream),
            "duplicate expectation for {id}/{stream}"
        );
        case.expectations.push(Expectation {
            stream: stream.clone(),
            text: block.script.clone(),
        });
    }
    Ok(cases)
}

fn fenced_body(block: &str) -> Result<String> {
    let lines: Vec<_> = block.lines().collect();
    let opening = lines.first().context("empty check")?.trim_start();
    let marker = opening.chars().next().context("empty fence")?;
    let count = opening.chars().take_while(|c| *c == marker).count();
    let last = lines.last().unwrap().trim();
    ensure!(
        lines.len() >= 2 && last.chars().all(|c| c == marker) && last.len() >= count,
        "unterminated check fence"
    );
    let start = block.find('\n').context("opening fence needs a newline")? + 1;
    let closing = block
        .trim_end_matches(['\r', '\n'])
        .rfind('\n')
        .context("closing fence needs its own line")?
        + 1;
    Ok(block[start..closing].to_owned())
}

fn parse_annotation(text: &str) -> Result<Annotation> {
    let mut entries = BTreeMap::new();
    for word in shell_words::split(text)? {
        let (key, value) = word
            .split_once('=')
            .context("annotation must be key=value")?;
        ensure!(
            ["xfail-on", "xfail-reason", "timeout"].contains(&key),
            "unknown annotation {key}"
        );
        ensure!(
            !value.is_empty() && entries.insert(key.to_owned(), value.to_owned()).is_none(),
            "empty/duplicate annotation {key}"
        );
    }
    let split = |name| -> Vec<String> {
        entries
            .get(name)
            .map(|v| v.split(',').map(str::to_owned).collect())
            .unwrap_or_default()
    };
    let xfail = split("xfail-on");
    ensure!(
        xfail.iter().all(|s| !s.is_empty()),
        "empty annotation list item"
    );
    let reason = entries.get("xfail-reason").cloned();
    let timeout = entries.get("timeout").cloned();
    if let Some(timeout) = &timeout {
        parse_timeout(timeout)?;
    }
    ensure!(
        xfail.is_empty() == reason.is_none(),
        "xfail-on and xfail-reason must appear together"
    );
    Ok(Annotation {
        xfail_on: xfail.into_iter().collect(),
        xfail_reason: reason,
        timeout,
    })
}
/// Use Bash's own syntax checker; specifications are trusted executable code.
pub fn lint_bash(source: &str) -> Result<()> {
    let output = std::process::Command::new("bash")
        .args(["--noprofile", "--norc", "-n", "-c", source])
        .output()
        .context("launch Bash syntax checker")?;
    ensure!(
        output.status.success(),
        "invalid bash syntax: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// Parse a positive per-case timeout with ms, s or m units.
pub fn parse_timeout(text: &str) -> Result<Duration> {
    let (value, multiplier) = if let Some(v) = text.strip_suffix("ms") {
        (v, 1)
    } else if let Some(v) = text.strip_suffix('s') {
        (v, 1000)
    } else if let Some(v) = text.strip_suffix('m') {
        (v, 60_000)
    } else {
        anyhow::bail!("timeout requires ms/s/m suffix");
    };
    let millis = value
        .parse::<u64>()?
        .checked_mul(multiplier)
        .ok_or_else(|| anyhow::anyhow!("timeout overflow"))?;
    ensure!(millis > 0, "timeout must be positive");
    Ok(Duration::from_millis(millis))
}
