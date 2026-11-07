#!/usr/bin/env bash
set -Eeuo pipefail
if [[ $# -gt 0 ]]; then echo "too many arguments for test-nightly-version" >&2; exit 2; fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/scripts/ci" "$work/pvisor" "$work/src"
cp "$MISE_PROJECT_ROOT/scripts/ci/set_nightly_local_version.py" "$work/scripts/ci/"
# Run the production task in an isolated, dependency-free project. No firmware,
# registry access or wheel compilation is needed to exercise both lockfiles.
python3 - "$MISE_PROJECT_ROOT" "$work" <<'PY'
import json
import sys
import tomllib
from pathlib import Path

root, work = map(Path, sys.argv[1:])
config = tomllib.loads((root / "mise.toml").read_text())
task = config["tasks"]["set-nightly-version"]
# Nested mise tasks resolve shims in this project, not in the parent repository.
# Declare the same tool versions so CI needs no global tool configuration.
tools = []
for name in ("python", "uv", "rust"):
    setting = config["tools"][name]
    version = setting["version"] if isinstance(setting, dict) else setting
    tools.append(f"{name} = {json.dumps(version)}\n")
(work / "mise.toml").write_text(
    "[tools]\n" + "".join(tools)
    + "[tasks.set-nightly-version]\nrun = " + json.dumps(task["run"]) + "\n"
)
(work / "pyproject.toml").write_text(
    '[project]\nname = "pvisor-sandbox"\nversion = "1.2.3"\n'
    'requires-python = ">=3.10"\n[dependency-groups]\ndev = []\n'
)
(work / "Cargo.toml").write_text(
    '[workspace]\nmembers = ["."]\n[workspace.package]\nversion = "1.2.3"\n'
    '[package]\nname = "nightly-fixture"\nversion.workspace = true\n'
)
(work / "pvisor/__init__.py").write_text('__version__ = "1.2.3"\n')
(work / "src/lib.rs").write_text("")
PY
mise trust "$work/mise.toml"
(
  cd "$work"
  cargo generate-lockfile --offline
  uv lock --offline
  mise run set-nightly-version -- g42.abcdef0
  cargo metadata --offline --locked --format-version 1 >/dev/null
  # Match the locked dev-tool invocation that used to fail in both wheel jobs.
  uv run --offline --locked --only-group dev python - <<'PY'
import tomllib
from pathlib import Path
from pvisor import __version__

expected = "1.2.3+g42.abcdef0"
assert __version__ == expected
for filename in ("Cargo.lock", "uv.lock"):
    packages = tomllib.loads(Path(filename).read_text())["package"]
    assert len(packages) == 1
    assert packages[0]["version"] == expected, filename
PY
)
echo 'PASS: nightly versions and both lockfiles support locked builds'
