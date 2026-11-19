#!/usr/bin/env bash
set -euo pipefail
if [[ $# -ne 2 || "$1" != docker-cli ]]; then
  echo "expected docker-cli bats|go-0|go-1" >&2
  exit 2
fi
suite="$2"
case "$suite" in
  bats|go-0|go-1) ;;
  *) echo "unknown contract shard: $suite" >&2; exit 2 ;;
esac
: "${PVISOR_BINARY:?set PVISOR_BINARY to a frozen runtime}"
: "${CONTRACT_LOGS:?set CONTRACT_LOGS to a private evidence directory}"
test -x "$PVISOR_BINARY"
mkdir -p "$CONTRACT_LOGS"
{
  uname -a
  "$PVISOR_BINARY" --version
  printf 'suite=%s\nPVISOR_TEST_IMAGE=%s\n' "$suite" "${PVISOR_TEST_IMAGE:-default}"
} | tee "$CONTRACT_LOGS/environment.txt"
if [[ "$suite" == bats ]]; then
  bats --version | tee -a "$CONTRACT_LOGS/environment.txt"
  bats --tap --print-output-on-failure tests/spec/docker-cli/regression/system 2>&1 | tee "$CONTRACT_LOGS/bats.tap"
else
  go version | tee -a "$CONTRACT_LOGS/environment.txt"
  go test -list '^Test' tests/spec/docker-cli/regression/e2e/*.go > "$CONTRACT_LOGS/discovered.txt"
  export CONTRACT_SHARD="${suite#go-}"
  python3 - <<'PY'
import hashlib
import os
import re
from pathlib import Path

logs = Path(os.environ["CONTRACT_LOGS"])
tests = [s for s in (logs / "discovered.txt").read_text().splitlines() if re.fullmatch(r"Test\w+", s)]
index = int(os.environ["CONTRACT_SHARD"])
selected = [s for s in tests if int.from_bytes(hashlib.sha256(s.encode()).digest(), "big") % 2 == index]
if not selected:
    raise SystemExit("empty contract shard")
(logs / "selected.txt").write_text("\n".join(selected) + "\n")
(logs / "filter.txt").write_text("^(" + "|".join(selected) + ")$")
PY
  # Populate the shard's immutable image data once; each test gets a private catalog.
  fixture=$(mktemp -d)
  trap 'rm -rf "$fixture"' EXIT
  export PVISOR_TEST_SEED_HOME="$fixture/seed"
  export PVISOR_IMAGE_STORE="$fixture/images"
  PVISOR_DOCKER_HOME="$PVISOR_TEST_SEED_HOME" "$PVISOR_BINARY" pull "${PVISOR_TEST_IMAGE:-docker.io/library/alpine:3.20}"
  go test -json -count=1 -timeout=20m -run "$(cat "$CONTRACT_LOGS/filter.txt")" tests/spec/docker-cli/regression/e2e/*.go 2>&1 | tee "$CONTRACT_LOGS/go.jsonl"
fi
