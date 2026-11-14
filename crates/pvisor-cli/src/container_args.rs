//! Shared host-side value parsing for Unix and Windows container frontends.
use anyhow::ensure;
use std::{collections::BTreeMap, io::Write, path::PathBuf};

pub(crate) fn batch(
    values: &[String],
    mut action: impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<i32> {
    let mut code = 0;
    for value in values {
        if let Err(error) = action(value) {
            eprintln!("Error: {error:#}");
            code = 1;
        }
    }
    Ok(code)
}

// Borrowed display inputs, not a second persistent container record.
pub(crate) struct PsRow<'a> {
    pub id: &'a str,
    pub image: &'a str,
    pub command: &'a [String],
    pub state: &'a str,
    pub exit_code: Option<i32>,
    pub ports: &'a str,
    pub name: &'a str,
}

pub(crate) fn ps_header(output: &mut impl Write, quiet: bool) -> std::io::Result<()> {
    if !quiet {
        writeln!(output, "CONTAINER ID\tIMAGE\tCOMMAND\tSTATUS\tPORTS\tNAMES")?;
    }
    Ok(())
}

pub(crate) fn ps_row(
    output: &mut impl Write,
    row: PsRow<'_>,
    quiet: bool,
    no_trunc: bool,
) -> std::io::Result<()> {
    let id = if no_trunc {
        row.id
    } else {
        &row.id[..row.id.len().min(12)]
    };
    if quiet {
        return writeln!(output, "{id}");
    }
    let status = match row.state {
        "exited" => format!("Exited ({})", row.exit_code.unwrap_or(125)),
        "running" => "Up".into(),
        state => state.into(),
    };
    writeln!(
        output,
        "{id}\t{}\t{:?}\t{status}\t{}\t{}",
        row.image,
        row.command.join(" "),
        row.ports,
        row.name
    )
}

#[cfg(any(windows, test))]
pub(crate) fn memory(value: &str) -> Result<u32, String> {
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let number: u64 = value[..split].parse().map_err(|_| "invalid memory size")?;
    let scale = match value[split..].to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1024,
        "m" | "mb" => 1024 * 1024,
        "g" | "gb" => 1024 * 1024 * 1024,
        _ => return Err("unsupported memory suffix".into()),
    };
    let bytes = number.checked_mul(scale).ok_or("memory size overflow")?;
    let mib = bytes.div_ceil(1024 * 1024);
    if mib < 16 {
        return Err("guest memory must be at least 16 MiB".into());
    }
    u32::try_from(mib).map_err(|_| "memory size overflow".into())
}

pub(crate) fn environment(
    values: &[String],
    files: &[PathBuf],
) -> anyhow::Result<(BTreeMap<String, String>, Vec<String>)> {
    let mut env = BTreeMap::new();
    let mut unset = std::collections::BTreeSet::new();
    let mut entries = Vec::new();
    for file in files {
        for line in std::fs::read_to_string(file)?.lines() {
            let line = line.trim_start();
            if !line.is_empty() && !line.starts_with('#') {
                entries.push(line.to_owned());
            }
        }
    }
    entries.extend_from_slice(values);
    for entry in entries {
        let (key, value) = match entry.split_once('=') {
            Some((key, value)) => (key, Some(value.to_owned())),
            None => (entry.as_str(), std::env::var(&entry).ok()),
        };
        ensure!(
            !key.is_empty() && !key.contains('\0'),
            "invalid environment name"
        );
        if let Some(value) = value {
            ensure!(!value.contains('\0'), "environment contains NUL");
            env.insert(key.into(), value);
            unset.remove(key);
        } else {
            env.remove(key);
            unset.insert(key.to_owned());
        }
    }
    Ok((env, unset.into_iter().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_rendering_and_batch_failures_share_the_same_contract() {
        let mut output = Vec::new();
        ps_header(&mut output, false).unwrap();
        let command = vec!["echo".into(), "hello world".into()];
        ps_row(
            &mut output,
            PsRow {
                id: "12345678901234567890123456789012",
                image: "alpine",
                command: &command,
                state: "running",
                exit_code: None,
                ports: "",
                name: "box",
            },
            false,
            false,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "CONTAINER ID\tIMAGE\tCOMMAND\tSTATUS\tPORTS\tNAMES\n123456789012\talpine\t\"echo hello world\"\tUp\t\tbox\n"
        );
        let mut quiet = Vec::new();
        for no_trunc in [false, true] {
            ps_row(
                &mut quiet,
                PsRow {
                    id: "12345678901234567890123456789012",
                    image: "alpine",
                    command: &command,
                    state: "exited",
                    exit_code: Some(7),
                    ports: "",
                    name: "box",
                },
                true,
                no_trunc,
            )
            .unwrap();
        }
        assert_eq!(
            String::from_utf8(quiet).unwrap(),
            "123456789012\n12345678901234567890123456789012\n"
        );
        let mut visited = Vec::new();
        let code = batch(&["missing".into(), "existing".into()], |value| {
            visited.push(value.to_owned());
            anyhow::ensure!(value != "missing", "no such container");
            Ok(())
        })
        .unwrap();
        assert_eq!(code, 1);
        assert_eq!(visited, ["missing", "existing"]);
    }

    #[test]
    fn memory_sizes_and_literal_environment_are_platform_independent() {
        assert_eq!(memory("512m").unwrap(), 512);
        assert_eq!(memory("1G").unwrap(), 1024);
        assert!(memory("0").is_err());
        assert!(memory("18446744073709551615g").is_err());
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("env");
        std::fs::write(&file, "# comment\r\nA=from-file\r\nB=a=b\r\n").unwrap();
        let (env, unset) = environment(&["A=literal $HOME".into()], &[file]).unwrap();
        assert_eq!(env["A"], "literal $HOME");
        assert_eq!(env["B"], "a=b");
        assert!(unset.is_empty());
    }
}
