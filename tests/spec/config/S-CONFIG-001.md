# S-CONFIG-001: Malformed configuration cannot launch a fallback command

**Origin**: New negative contract; S-DOC-043 through S-DOC-045 remain unchanged.

**Contract**: Invalid TOML and invalid JSON RunSpec fail with diagnostics, do not
execute the supplied command, do not overwrite a pre-existing result file, and
do not publish a Session. Missing input is also an error, not a fallback launch.

**Prerequisites**: Executable `SUBJECT_BIN` and Bash. No guest or FUSE is used.
This covers parse failure only, not rollback of a successfully admitted run.

```scrut {output_stream: combined, timeout: 40s}
$ python3 "$TESTDIR/../fixture.py" use S-CONFIG-001 --timeout 30 <<'CASE'
> test -x "$SUBJECT_BIN"
> printf '[run\ncommand = ["touch", "must-not-exist"]\n' > broken.toml
> printf '{"invocation": invalid json}\n' > broken.json
> printf 'previous result\n' > result.json
> cp result.json expected
> expect_refused pvisor --config broken.toml
> expect_refused pvisor --spec broken.json --result-file result.json
> expect_refused pvisor --config absent.toml
> cmp expected result.json
> assert_absent must-not-exist
> test -z "$(find "$PVISOR_RUN_HOME" -mindepth 1 -print)"
> CASE
PASS S-CONFIG-001
```
