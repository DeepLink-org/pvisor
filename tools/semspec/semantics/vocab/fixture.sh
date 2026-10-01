# Ephemeral test data only. This never approves a real specification.
fixture() {
  mkdir -p "$CASE_ROOT/project/spec/vocab"
  cat > "$CASE_ROOT/project/semspec.toml" <<'TOML'
[project]
name = "fixture"
spec_dirs = ["spec"]
ledger = "REVIEWED.toml"
approved_snapshots = ".approved"
retired = []
[subject]
bin = "/usr/bin/true"
language = "bash"
vocab = ["spec/vocab/fixture.sh"]
env = {}
timeout = "1s"
[platforms]
macos = { os = "macos" }
linux = { os = "linux" }
TOML
  printf 'noop() { :; }\n' > "$CASE_ROOT/project/spec/vocab/fixture.sh"
  cat > "$CASE_ROOT/project/spec/case.md" <<'SPEC'
### S-FIXTURE-001: preserve

**语义**: preserve the result.

**违反示例**: alter the result.

```bash
true
```
SPEC
  # Independent digest calculation for a simulated human approval in test data.
  python3 - "$CASE_ROOT/project" <<'PY'
import hashlib, pathlib, sys
root = pathlib.Path(sys.argv[1])
normalize = lambda s: ('\n'.join(line.rstrip() for line in s.splitlines()).rstrip('\n') + '\n').encode()
sha = lambda b: 'sha256:' + hashlib.sha256(b).hexdigest()
vocab = sha(b'semspec/vocab/v1\0fixture.sh\0' + normalize((root/'spec/vocab/fixture.sh').read_text()))
case = sha(b'semspec/case/v1\0' + normalize((root/'spec/case.md').read_text()) + b'\0' + vocab.encode() + b'\0' + b'1')
(root/'REVIEWED.toml').write_text(f'format = 1\n[[approval]]\nitem = "S-FIXTURE-001"\ndigest = "{case}"\nreviewer = "simulated-fixture"\ndate = "2026-10-01"\n')
PY
}
fixture_state() { "$SUBJECT_BIN" --config "$CASE_ROOT/project/semspec.toml" list; }
