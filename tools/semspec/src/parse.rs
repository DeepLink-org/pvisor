use crate::{
    model::{Annotation, Case, valid_case_id},
    seal::normalize,
};
use anyhow::{Context, Result, bail, ensure};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Parser, Tag, TagEnd};
use std::{collections::BTreeMap, path::Path};

pub fn parse_spec(path: &Path, source: &str) -> Result<Vec<Case>> {
    let mut headings = Vec::new();
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
    let mut cases = Vec::new();
    for (i, (start, level, title)) in headings.iter().enumerate() {
        if *level != HeadingLevel::H3 || !title.starts_with("S-") {
            continue;
        }
        let end = headings[i + 1..]
            .iter()
            .find(|(_, l, _)| *l <= HeadingLevel::H3)
            .map(|(offset, _, _)| *offset)
            .unwrap_or(source.len());
        let line = source[..*start].bytes().filter(|c| *c == b'\n').count() + 1;
        cases.push(
            parse_case(path, line, title, &source[*start..end])
                .with_context(|| format!("{}:{line}", path.display()))?,
        );
    }
    Ok(cases)
}
fn parse_case(path: &Path, line: usize, title: &str, text: &str) -> Result<Case> {
    let normalized = normalize(text);
    let text = normalized.as_str();
    let (id, title) = title
        .split_once([':', '：'])
        .context("case heading needs ID: title")?;
    ensure!(
        valid_case_id(id) && !title.trim().is_empty(),
        "invalid case ID/title"
    );
    let mut prose = BTreeMap::new();
    let mut script = None;
    let mut annotation = None;
    for (event, range) in Parser::new(text).into_offset_iter() {
        match event {
            Event::Start(Tag::Paragraph) => {
                let paragraph = &text[range];
                for label in ["语义", "违反示例", "理由"] {
                    if let Some(value) = paragraph.strip_prefix(&format!("**{label}**")) {
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
            Event::Start(Tag::CodeBlock(kind)) => {
                ensure!(
                    matches!(&kind, CodeBlockKind::Fenced(lang) if lang.as_ref()=="bash"),
                    "only bash fenced checks are supported"
                );
                ensure!(script.is_none(), "case requires exactly one check");
                let block = &text[range];
                let lines: Vec<_> = block.lines().collect();
                let opening = lines.first().context("empty check")?.trim_start();
                let marker = opening.chars().next().unwrap();
                let count = opening.chars().take_while(|c| *c == marker).count();
                let last = lines.last().unwrap().trim();
                ensure!(
                    lines.len() >= 2 && last.chars().all(|c| c == marker) && last.len() >= count,
                    "unterminated check fence"
                );
                let body = format!("{}\n", lines[1..lines.len() - 1].join("\n"));
                lint_bash(&body)?;
                script = Some(body);
            }
            Event::Html(html) | Event::InlineHtml(html) if html.contains("semantic-case:") => {
                ensure!(
                    annotation.is_none() && html.trim().lines().count() == 1,
                    "annotation must be a single unique line"
                );
                let body = html
                    .trim()
                    .strip_prefix("<!--")
                    .and_then(|s| s.strip_suffix("-->"))
                    .context("invalid annotation")?
                    .trim()
                    .strip_prefix("semantic-case:")
                    .context("invalid annotation")?;
                annotation = Some(parse_annotation(body)?);
            }
            _ => {}
        }
    }
    let claim = prose.remove("语义").context("missing **语义** paragraph")?;
    let violation = prose
        .remove("违反示例")
        .context("missing **违反示例** paragraph")?;
    Ok(Case {
        id: id.into(),
        domain: id.split('-').nth(1).unwrap().into(),
        title: title.trim().into(),
        file: path.into(),
        start_line: line,
        end_line: line + text.lines().count().saturating_sub(1),
        text: normalize(text),
        claim,
        violation,
        rationale: prose.remove("理由"),
        script: script.context("case requires exactly one bash check")?,
        annotation: annotation.unwrap_or_default(),
    })
}
fn parse_annotation(text: &str) -> Result<Annotation> {
    let mut entries = BTreeMap::new();
    for word in shell_words::split(text)? {
        let (key, value) = word
            .split_once('=')
            .context("annotation must be key=value")?;
        ensure!(
            ["requires", "xfail-on", "xfail-reason", "vocab"].contains(&key),
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
    let requires = split("requires");
    let xfail = split("xfail-on");
    let vocab = split("vocab");
    ensure!(
        requires
            .iter()
            .chain(&xfail)
            .chain(&vocab)
            .all(|s| !s.is_empty()),
        "empty annotation list item"
    );
    let reason = entries.get("xfail-reason").cloned();
    ensure!(
        xfail.is_empty() == reason.is_none(),
        "xfail-on and xfail-reason must appear together"
    );
    Ok(Annotation {
        requires: requires.into_iter().collect(),
        xfail_on: xfail.into_iter().collect(),
        xfail_reason: reason,
        vocab: entries.contains_key("vocab").then_some(vocab),
    })
}
/// Audit-scope lint, not a sandbox. Dynamic shell programs remain trusted code.
pub fn lint_bash(source: &str) -> Result<()> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_bash::LANGUAGE.into())?;
    let tree = parser.parse(source, None).context("Bash parser failed")?;
    ensure!(!tree.root_node().has_error(), "invalid bash syntax");
    fn visit(node: tree_sitter::Node<'_>, source: &str) -> Result<()> {
        if node.kind() == "command"
            && let Some(name) = node.child_by_field_name("name")
        {
            let name = shell_words::split(name.utf8_text(source.as_bytes())?)?;
            let mut name = name.first().map(String::as_str).unwrap_or("");
            if ["builtin", "command"].contains(&name) {
                if let Some(argument) = node.child_by_field_name("argument") {
                    let argument = shell_words::split(argument.utf8_text(source.as_bytes())?)?;
                    if argument.first().is_some_and(|s| s == "source" || s == ".") {
                        bail!("source/dot commands are outside sealed vocabulary");
                    }
                }
                name = "";
            }
            ensure!(
                name != "source" && name != ".",
                "source/dot commands are outside sealed vocabulary"
            );
        }
        if node.kind() == "variable_assignment"
            && let Some(name) = node.child_by_field_name("name")
        {
            ensure!(
                !name.utf8_text(source.as_bytes())?.starts_with("SEMSPEC_"),
                "SEMSPEC_* variables are reserved"
            );
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            visit(child, source)?;
        }
        Ok(())
    }
    visit(tree.root_node(), source)
}
