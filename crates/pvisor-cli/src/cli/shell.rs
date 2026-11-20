use std::path::Path;

pub(super) fn quote_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "-._/".contains(ch))
    {
        value.into_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_words_round_trip_through_the_posix_shell() {
        for value in [
            "",
            "/tmp/plain-stage",
            "/tmp/stage with spaces",
            "/tmp/owner's stage",
            "/tmp/$literal[1]*",
            "/tmp/双引号\"和单引号'",
            "/tmp/line\nbreak",
        ] {
            let output = std::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    &format!("printf '%s' {}", quote_path(Path::new(value))),
                ])
                .output()
                .unwrap();
            assert!(output.status.success(), "{value:?}: {:?}", output.stderr);
            assert_eq!(output.stdout, value.as_bytes(), "{value:?}");
        }
        assert_eq!(
            quote_path(Path::new("/tmp/plain-stage")),
            "/tmp/plain-stage"
        );
    }
}
