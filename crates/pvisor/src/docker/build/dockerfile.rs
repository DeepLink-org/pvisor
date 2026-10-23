use anyhow::{Context, bail, ensure};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(super) struct Instruction {
    pub line: usize,
    pub name: String,
    pub value: String,
}

pub(super) fn parse(text: &str) -> anyhow::Result<Vec<Instruction>> {
    ensure!(!text.contains('\0'), "Dockerfile contains NUL");
    let mut result = Vec::new();
    let mut pending = String::new();
    let mut start = 0;
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            ensure!(
                !line.starts_with("# escape=") || line == "# escape=\\",
                "only backslash Dockerfile escape is supported"
            );
            continue;
        }
        if pending.is_empty() {
            start = index + 1;
        }
        let continued = line.ends_with('\\');
        pending.push_str(if continued {
            &line[..line.len() - 1]
        } else {
            line
        });
        if continued {
            pending.push(' ');
            continue;
        }
        let (name, value) = pending
            .split_once(char::is_whitespace)
            .context("Dockerfile instruction requires an argument")?;
        let name = name.to_ascii_uppercase();
        ensure!(
            matches!(
                name.as_str(),
                "FROM"
                    | "RUN"
                    | "COPY"
                    | "ARG"
                    | "ENV"
                    | "WORKDIR"
                    | "USER"
                    | "CMD"
                    | "ENTRYPOINT"
                    | "SHELL"
                    | "LABEL"
                    | "EXPOSE"
                    | "VOLUME"
                    | "STOPSIGNAL"
            ),
            "Dockerfile line {start}: unsupported instruction {name}"
        );
        let value = value.trim().to_owned();
        ensure!(
            !value.is_empty() && !value.contains("<<"),
            "Dockerfile line {start}: empty argument or unsupported heredoc"
        );
        if name == "RUN" {
            ensure!(
                !value.starts_with("--"),
                "RUN flags/mounts are not supported"
            );
        }
        result.push(Instruction {
            line: start,
            name,
            value,
        });
        pending.clear();
    }
    ensure!(pending.is_empty(), "unterminated Dockerfile continuation");
    ensure!(
        result.iter().any(|i| i.name == "FROM"),
        "Dockerfile requires FROM"
    );
    Ok(result)
}

// Tokenize Dockerfile arguments, not shell commands. RUN shell text stays intact.
pub(super) fn words(value: &str) -> anyhow::Result<Vec<String>> {
    lex(value, false)
}

pub(super) fn expanded_words(
    value: &str,
    env: &BTreeMap<String, String>,
) -> anyhow::Result<Vec<String>> {
    lex(value, true)?
        .into_iter()
        .map(|v| expand(&v, env))
        .collect()
}

fn lex(value: &str, protect_literals: bool) -> anyhow::Result<Vec<String>> {
    if value.trim_start().starts_with('[') {
        return serde_json::from_str(value).context("invalid Dockerfile JSON string array");
    }
    let mut result = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escape = false;
    let mut active = false;
    for c in value.chars() {
        if escape {
            if protect_literals && (c == '$' || c == '\\') {
                word.push('\\');
            }
            word.push(c);
            escape = false;
            active = true;
            continue;
        }
        if c == '\\' && quote != Some('\'') {
            escape = true;
            active = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                if protect_literals && q == '\'' && (c == '$' || c == '\\') {
                    word.push('\\');
                }
                word.push(c);
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
            active = true;
        } else if c.is_whitespace() {
            if active {
                result.push(std::mem::take(&mut word));
                active = false;
            }
        } else {
            word.push(c);
            active = true;
        }
    }
    ensure!(
        !escape && quote.is_none(),
        "unterminated Dockerfile quoting"
    );
    if active {
        result.push(word);
    }
    Ok(result)
}

pub(super) fn expand(value: &str, env: &BTreeMap<String, String>) -> anyhow::Result<String> {
    let mut out = String::new();
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(c) = chars.next() {
                out.push(c);
            } else {
                out.push('\\');
            }
            continue;
        }
        if c != '$' {
            out.push(c);
            continue;
        }
        let mut name = String::new();
        if chars.peek() == Some(&'{') {
            chars.next();
            loop {
                match chars.next() {
                    Some('}') => break,
                    Some(c) => name.push(c),
                    None => bail!("unterminated variable expansion"),
                }
            }
            ensure!(
                name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "extended variable substitution is unsupported: ${{{name}}}"
            );
        } else {
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
            {
                name.push(chars.next().unwrap());
            }
        }
        if name.is_empty() {
            out.push('$');
        } else {
            out.push_str(env.get(&name).map(String::as_str).unwrap_or(""));
        }
    }
    Ok(out)
}

pub(super) fn assignments(
    value: &str,
    env: &BTreeMap<String, String>,
) -> anyhow::Result<Vec<(String, String)>> {
    let parts = expanded_words(value, env)?;
    ensure!(!parts.is_empty(), "missing assignment");
    let pairs = if parts[0].contains('=') {
        parts
            .iter()
            .map(|p| {
                p.split_once('=')
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .context("expected NAME=value")
            })
            .collect::<anyhow::Result<Vec<_>>>()?
    } else {
        ensure!(parts.len() >= 2, "expected NAME value");
        vec![(parts[0].clone(), parts[1..].join(" "))]
    };
    ensure!(
        pairs.iter().all(|(k, _)| valid_variable(k)),
        "invalid variable/label name"
    );
    Ok(pairs)
}

pub(super) fn valid_variable(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .enumerate()
            .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || i > 0 && c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_continuations_and_literal_json_without_shell_execution() {
        let p=parse("ARG BASE=alpine\nFROM $BASE AS build\nRUN echo x \\\n&& echo y\nCOPY [\"a b\",\"/out/\"]\n").unwrap();
        assert_eq!(p[2].value, "echo x  && echo y");
        assert_eq!(words(&p[3].value).unwrap(), ["a b", "/out/"]);
        assert!(parse("FROM scratch\nRUN --mount=type=secret x").is_err());
        assert!(words("\"unterminated").is_err());
        assert!(expand("${X:-fallback}", &BTreeMap::new()).is_err());
        assert_eq!(
            expand("$X/${Y}", &BTreeMap::from([("X".into(), "a".into())])).unwrap(),
            "a/"
        );
    }
}
