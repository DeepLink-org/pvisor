//! Source-preserving Markdown transformation. Rendering never implies execution.
use crate::{experiment, model::RunReport, parse, project::Project, table};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Serialize)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}
#[derive(Serialize)]
pub struct Diagnostic {
    pub line: usize,
    pub message: String,
}
#[derive(Serialize)]
pub struct Document {
    pub format: u32,
    pub source: PathBuf,
    pub markdown: String,
    pub dependencies: Vec<PathBuf>,
    /// Validated benchmark plans in source order; inventory only, never execution.
    pub experiments: Vec<experiment::Cell>,
    /// Byte offsets in the original input, before any semspec substitutions.
    pub edits: Vec<Edit>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Fence arbitrary output without allowing it to escape into active Markdown/HTML.
pub fn code(text: &str) -> String {
    let fence = "`".repeat(
        text.split(|c| c != '`')
            .map(str::len)
            .max()
            .unwrap_or(0)
            .max(2)
            + 1,
    );
    format!(
        "{fence}text\n{text}{}{fence}\n",
        if text.ends_with('\n') { "" } else { "\n" }
    )
}

pub fn render(
    source: &str,
    path: &Path,
    project: Option<&Project>,
    report: Option<&RunReport>,
    strict: bool,
    best_effort: bool,
) -> Result<Document> {
    let cases = parse::parse_document(path, source)?;
    let blocks = parse::marked_blocks(source)?;
    let experiments = experiment::cells(source)?;
    let mut edits = Vec::new();
    let mut diagnostics = Vec::new();
    let mut dependencies = BTreeSet::from([path.to_path_buf()]);
    if let Some(project) = project {
        for name in project.preparation.keys() {
            dependencies.insert(project.root.join(name));
        }
        dependencies.insert(project.root.join("REVIEWED.toml"));
    }
    if let Some(report) = report {
        let ids: BTreeSet<_> = report.results.iter().map(|r| &r.id).collect();
        ensure!(ids.len() == report.results.len(), "duplicate case results");
        let ids: BTreeSet<_> = report.benchmarks.iter().map(|r| &r.id).collect();
        ensure!(
            ids.len() == report.benchmarks.len(),
            "duplicate benchmark results"
        );
    }
    for block in blocks {
        // Remove only our annotation line. Preserve every other source byte.
        let end = source[block.start..]
            .find('\n')
            .map(|n| block.start + n + 1)
            .unwrap_or(source.len());
        edits.push(Edit {
            start: block.start,
            end,
            text: String::new(),
        });
        if block.parameters.starts_with("bench ") {
            let cell = experiments.iter().find(|c| c.end == block.end).unwrap();
            let row = report.and_then(|r| r.benchmarks.iter().find(|r| r.id == cell.id));
            let fresh = row.is_some_and(|r| r.digest == cell.digest);
            let state = if fresh {
                row.unwrap().data["status"].as_str().unwrap_or("ERROR")
            } else if row.is_some() {
                "STALE"
            } else {
                "NOT RUN"
            };
            if !fresh {
                diagnostics.push(Diagnostic {
                    line: source[..block.start].lines().count() + 1,
                    message: format!("{}: {state}", cell.id),
                });
                ensure!(!strict, "{}: {}: {state}", path.display(), cell.id);
            }
            let mut text = format!(
                "\n> **{} — {state}** · observation; not human approval\n\n",
                cell.id
            );
            if fresh {
                let data = &row.unwrap().data;
                if state == "PASS" {
                    text.push_str("| N | Median ms | Mean ms | SD ms | Min ms | Max ms |\n| ---: | ---: | ---: | ---: | ---: | ---: |\n|");
                    for key in ["N", "Median ms", "Mean ms", "SD ms", "Min ms", "Max ms"] {
                        text.push_str(&format!(" {} |", data["result"][key]));
                    }
                    text.push_str("\n\n");
                } else {
                    text.push_str(&code(data["error"].as_str().unwrap_or("experiment failed")));
                }
            }
            edits.push(Edit {
                start: block.end,
                end: block.end,
                text,
            });
            continue;
        }
        if !block.parameters.starts_with("case ") {
            continue;
        }
        let case = cases
            .iter()
            .find(|c| c.end_line == source[..block.end].lines().count())
            .context("case source range missing")?;
        let current =
            project.and_then(|p| p.cases.iter().find(|c| c.id == case.id).map(|c| (p, c)));
        let review = current
            .map(|(p, c)| p.item(&c.id).map(|i| i.review.label()))
            .transpose()?
            .unwrap_or("UNREVIEWED");
        let row = report.and_then(|r| r.results.iter().find(|r| r.id == case.id));
        let fresh = match (current, row.and_then(|r| r.evidence.as_ref())) {
            (Some((p, canonical)), Some(evidence)) => {
                // Evidence binds the rendered claim and the exact executable cells.
                report.unwrap().engine_semantics == crate::seal::ENGINE_SEMANTICS
                    && p.case_digest(canonical) == evidence.digest
                    && (!matches!(
                        row.unwrap().verdict,
                        crate::model::Verdict::Pass | crate::model::Verdict::XPass
                    ) || (evidence.exit_code == Some(0)
                        && !evidence.timed_out
                        && evidence.cancelled.is_none()))
                    && canonical.text == case.text
                    && canonical.script == case.script
                    && serde_json::to_string(&canonical.expectations)?
                        == serde_json::to_string(&case.expectations)?
            }
            _ => false,
        };
        let state = if row.is_some() && !fresh {
            "STALE"
        } else if let Some(row) = row {
            row.verdict.label()
        } else {
            "NOT RUN"
        };
        if !fresh {
            diagnostics.push(Diagnostic {
                line: case.start_line,
                message: format!("{}: {state}", case.id),
            });
            ensure!(
                !strict,
                "{}:{}: {}: {state}",
                path.display(),
                case.start_line,
                case.id
            );
        }
        let mut text = format!("\n> **{} — {state}** · case review: {review}\n\n", case.id);
        if let Some((project, canonical)) = current {
            text.push_str(&format!(
                "> engine {} review: {}",
                crate::seal::ENGINE_SEMANTICS,
                project.item("@engine")?.review.label()
            ));
            for name in &canonical.preparation {
                text.push_str(&format!(
                    " · preparation `{name}`: {}",
                    project.item(&format!("@vocab:{name}"))?.review.label()
                ));
            }
            text.push_str("\n\n");
        }
        if fresh {
            let row = row.unwrap();
            let evidence = row.evidence.as_ref().unwrap();
            let detail = match &row.verdict {
                crate::model::Verdict::Fail { output_tail } => Some(output_tail),
                crate::model::Verdict::Error { message } => Some(message),
                crate::model::Verdict::Skip { reason }
                | crate::model::Verdict::XFail { reason, .. } => Some(reason),
                _ => None,
            };
            if let Some(detail) = detail.filter(|s| !s.is_empty()) {
                text.push_str(&format!("diagnostic:\n\n{}\n", code(detail)));
            }
            for (name, output) in [("stdout", &evidence.stdout), ("stderr", &evidence.stderr)] {
                if !output.is_empty() {
                    text.push_str(&format!("{name}:\n\n{}\n", code(output)));
                }
            }
            if evidence.truncated {
                text.push_str("> Output preview truncated; full streams remain in the retained case directory when requested.\n\n");
            }
            text.push_str(&format!(
                "> platform: {} · elapsed: {} ms · exit: {:?}\n\n",
                report.unwrap().platform,
                row.duration_ms,
                evidence.exit_code
            ));
        }
        edits.push(Edit {
            start: block.end,
            end: block.end,
            text,
        });
    }
    edits.sort_by_key(|edit| (edit.start, edit.end));
    let mut markdown = String::new();
    let mut cursor = 0;
    for edit in &edits {
        ensure!(edit.start >= cursor, "overlapping document edits");
        markdown.push_str(&source[cursor..edit.start]);
        markdown.push_str(&edit.text);
        cursor = edit.end;
    }
    markdown.push_str(&source[cursor..]);
    // Track table dependencies using Markdown events, never scan inside examples.
    for line in match table::references(source) {
        Ok(paths) => paths,
        Err(_) if best_effort => Vec::new(),
        Err(error) => return Err(error),
    } {
        dependencies.insert(path.parent().unwrap_or(Path::new(".")).join(line));
    }
    let tables = table::render(&markdown, path, best_effort)?;
    if tables != markdown {
        // A table may change byte offsets after case insertions. A single source
        // replacement keeps the edit protocol exact for adapters applying patches.
        edits = vec![Edit {
            start: 0,
            end: source.len(),
            text: tables.clone(),
        }];
    }
    markdown = tables;
    Ok(Document {
        format: 1,
        source: path.to_path_buf(),
        markdown,
        dependencies: dependencies.into_iter().collect(),
        experiments,
        edits,
        diagnostics,
    })
}
