#!/usr/bin/env bash
set -Eeuo pipefail
if [[ $# -gt 0 ]]; then echo "too many arguments for test-task-routing" >&2; exit 2; fi
work=$(mktemp -d)
work=$(cd "$work" && pwd -P)
trap 'echo "FAIL: $BASH_COMMAND" >&2; cat "$work/stdout" "$work/stderr" >&2 2>/dev/null || true' ERR
trap 'rm -rf "$work"' EXIT
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) python_binary="$(mise where python)/python.exe" ;;
  *) python_binary="$(mise where python)/bin/python3" ;;
esac
test "$UV_PYTHON" == "$python_binary"
uv run --no-project python -c 'import os, sys; assert os.path.samefile(sys.executable, os.environ["UV_PYTHON"])'
# A fresh tool store must allow inspection before installation.
MISE_DATA_DIR="$work/uninstalled-tools" mise -C "$MISE_PROJECT_ROOT" task fmt > "$work/task-info"
grep -Fq 'Task: fmt' "$work/task-info"
MISE_DATA_DIR="$work/uninstalled-tools" mise -C "$MISE_PROJECT_ROOT" env --json > "$work/env.json"
jq -e '.UV_PYTHON == "python3"' "$work/env.json" > /dev/null
# Run a release check with only Python present; nested task startup must not
# install Rust, Zig or the Cargo plugins in an explicitly provisioned CI session.
python_install=$(mise where python)
mkdir -p "$work/python-only/installs/python"
ln -s "$python_install" "$work/python-only/installs/python/${python_install##*/}"
MISE_DATA_DIR="$work/python-only" MISE_AUTO_INSTALL=false mise -C "$MISE_PROJECT_ROOT" run check-release-artifacts -- --help > "$work/release-help"
grep -Fq -- '--version' "$work/release-help"
test "$(find "$work/python-only/installs" -mindepth 1 -maxdepth 1 -type d | wc -l)" -eq 1
# Check precedence for installed plugins; lightweight CI need not install them.
for plugin in github:nextest-rs/nextest cargo:cargo-zigbuild; do
  if ! installation=$(mise where "$plugin" 2>/dev/null); then continue; fi
  binary=cargo-nextest
  if [[ "$plugin" == cargo:cargo-zigbuild ]]; then binary=cargo-zigbuild; fi
  case "$(command -v "$binary")" in
    "$installation"/*) ;;
    *) echo "$binary is outside the mise installation" >&2; exit 1 ;;
  esac
done
# Routing uses file selection; native Scrut parsing has its own fixture gate.
export PVISOR_TASK_TEST_LOG="$work/commands" CARGO_TARGET_DIR="$work/target with spaces" CARGO_INSTALL_ROOT="$work/install"
export PVISOR_TASK_TEST_PYTHON="$(python3 -c 'import sys; print(sys.executable)')"
mkdir "$work/bin" "$work/tests"
mkdir -p "$work/scripts/tasks" "$work/tests/tasks"
cp "$MISE_PROJECT_ROOT"/scripts/tasks/*.sh "$work/scripts/tasks/"
cp "$MISE_PROJECT_ROOT"/tests/tasks/*.sh "$work/tests/tasks/"
cp -R "$MISE_PROJECT_ROOT/tests/spec" "$work/tests/spec"
# Exercise the native tasks with stub tools, without installing a toolchain.
sed '/^\[tools\]/,/^\[task_config\]/{ /^\[task_config\]/!d; }; /^UV_PYTHON =/d' "$MISE_PROJECT_ROOT/mise.toml" > "$work/mise.toml"
cp "$MISE_PROJECT_ROOT/tests/tasks.toml" "$work/tests/tasks.toml"
mise trust "$work/mise.toml"
cat > "$work/bin/tool" <<'SH'
#!/usr/bin/env bash
set -Eeuo pipefail
name=${0##*/}
printf '%q\t' "$name" "$@" >> "$PVISOR_TASK_TEST_LOG"
printf '\n' >> "$PVISOR_TASK_TEST_LOG"
args=("$@")
if [[ "$name" == python3 && "${2:-}" == --check ]]; then
  exec "$PVISOR_TASK_TEST_PYTHON" "$@"
fi
if [[ "$name" == python3 && "$1" == */tests/spec/report.py ]]; then
  exec "$PVISOR_TASK_TEST_PYTHON" "$@"
fi
if [[ "$name" == python3 && "$1" == - ]]; then
  exec "$PVISOR_TASK_TEST_PYTHON" "$@"
fi
if [[ "$name" == go && "${2:-}" == -list ]]; then
  printf '%s\n' TestAlpha TestBeta TestNewCase Test4
fi
if [[ ( "$name" == go || "$name" == bats ) && "${PVISOR_TASK_TEST_BAD_RUN:-0}" == 1 ]]; then exit 1; fi
if [[ "$name" == cargo && "$1" == install && "$2" == scrut ]]; then
  mkdir -p "$CARGO_TARGET_DIR/scrut/bin"
  cp "${PVISOR_TASK_TEST_LOG%/*}/bin/tool" "$CARGO_TARGET_DIR/scrut/bin/scrut"
fi
if [[ "$name" == scrut ]]; then
  printf '%s\n' "${PVISOR_CASE_REQUIRE_PASS:-0}" > "$PVISOR_TASK_TEST_LOG.strict"
  if [[ "${PVISOR_TASK_TEST_BAD_RUN:-0}" == 1 ]]; then exit 1; fi
  exec "$PVISOR_TASK_TEST_PYTHON" -c 'import json,sys; print(json.dumps([{"location":p,"result":{"kind":"success"}} for p in sys.argv[4:]]))' "$@"
fi
if [[ "$name" == python3 && "$1" == scripts/check-docs.py && "${PVISOR_TASK_TEST_BAD_DOCS_PREFLIGHT:-0}" == 1 ]]; then exit 1; fi
if [[ "$name" == python3 && "$1" == scripts/packaging/stage_wheel_binaries.py ]]; then
  if [[ "${PVISOR_TASK_TEST_BAD_BUILD:-0}" == 1 ]]; then exit 1; fi
  profile=debug
  for ((i=0; i<$#; i++)); do
    if [[ "${args[$i]}" == --profile ]]; then profile=${args[$((i+1))]}; fi
  done
  if [[ "$profile" == dev ]]; then profile=debug; fi
  names=(pvisor pvisor-replay pvisor-daemon)
  for arg in "$@"; do
    case "$arg" in
      --all) names+=(pvisor-guest containerd-shim-pvisor-v2) ;;
      --daemon) names=(pvisor-daemon) ;;
      --shim|--shim-vm) names=(containerd-shim-pvisor-v2) ;;
    esac
  done
  mkdir -p "$CARGO_TARGET_DIR/$profile"
  for binary in "${names[@]}"; do
    printf '#!/bin/sh\nexit 0\n' > "$CARGO_TARGET_DIR/$profile/$binary"
    chmod +x "$CARGO_TARGET_DIR/$profile/$binary"
  done
fi
if [[ "$name" == cargo && "$1" == nextest && "$2" == list ]]; then
  if [[ "${PVISOR_TASK_TEST_BAD_LIST:-0}" == 1 ]]; then exit 1; fi
  jq -n --arg directory "${PVISOR_TASK_TEST_LOG%/*}/tests with spaces" '{"rust-suites": (["pvisor-vm", "pvisor", "pvisor-cli", "nativepvisor", "pvisor-daemon", "pvisor-core"] | map({key: ., value: {"package-name": ., "binary-path": ($directory + "/" + . + "\nfixture")}}) | from_entries)}'
fi
if [[ "$name" == cargo && "$1" == nextest && "$2" == run && "${PVISOR_TASK_TEST_BAD_NATIVE:-0}" == 1 ]]; then exit 1; fi
if [[ "$name" == cargo && "$1" == run ]]; then
  for ((i=0; i<$#; i++)); do
    if [[ "${args[$i]}" == --manifest-path && "${args[$((i+1))]}" == */benchmark/criterion/Cargo.toml ]]; then
      # Cargo owns build failure; the native CLI owns argument refusal. Neither
      # stub path executes an experiment or evaluates any forwarded argument.
      if [[ "${PVISOR_TASK_TEST_BAD_BUILD:-0}" == 1 ]]; then exit 1; fi
      for arg in "$@"; do
        if [[ "$arg" == unknown ]]; then
          echo "error: unexpected argument 'unknown'" >&2
          exit 2
        fi
      done
      break
    fi
  done
fi
if [[ "$name" == cargo && "$1" == run && "${PVISOR_TASK_TEST_BAD_RUN:-0}" == 1 ]]; then exit 1; fi
if [[ "$name" == timeout && "${PVISOR_TASK_TEST_BAD_ISOLATION:-0}" == 1 ]]; then exit 1; fi
if [[ "$name" == codesign && "${PVISOR_TASK_TEST_BAD_CODESIGN:-0}" == 1 ]]; then exit 1; fi
if [[ "$name" == codesign && "$1" == --verify && "${PVISOR_TASK_TEST_BAD_VERIFY:-0}" == 1 ]]; then exit 1; fi
if [[ "$name" == cargo && "$1" == build ]]; then
  if [[ "${PVISOR_TASK_TEST_BAD_BUILD:-0}" == 1 ]]; then exit 1; fi
  profile=debug
  for ((i=0; i<$#; i++)); do
    if [[ "${args[$i]}" == --release ]]; then profile=release; fi
    if [[ "${args[$i]}" == --example ]]; then example=${args[$((i+1))]}; fi
  done
  if [[ -n "${example:-}" ]]; then
    mkdir -p "$CARGO_TARGET_DIR/$profile/examples"
    printf '#!/bin/sh\nexit 0\n' > "$CARGO_TARGET_DIR/$profile/examples/$example"
    chmod +x "$CARGO_TARGET_DIR/$profile/examples/$example"
  fi
fi
if [[ "$name" == uv && "$1" == sync && "${PVISOR_TASK_TEST_BAD_SYNC:-0}" == 1 ]]; then exit 1; fi
  if [[ "$name" == uv || "$name" == uvx ]]; then
  if [[ " $* " == *" zensical serve "* ]]; then test "${PVISOR_DOCS_LIVE:-0}" == 1; fi
  if [[ " $* " == *" zensical build "* ]]; then test "${PVISOR_DOCS_LIVE:-0}" == 0; fi
  for ((i=0; i<$#; i++)); do
    if [[ "${args[$i]}" == --out-dir || "${args[$i]}" == --output-dir ]]; then
      staging=${args[$((i+1))]}
      touch "$staging/pvisor_sandbox-a.whl" "$staging/pvisor_sandbox-b.whl" "$staging/pvisor-firmware-source-linux-x86_64.tar.gz"
    fi
  done
fi
if [[ "$name" == python3 && "$1" == scripts/packaging/verify_wheel.py && "${PVISOR_TASK_TEST_BAD_WHEEL:-0}" == 1 && "$2" == */pvisor_sandbox-b.whl ]]; then exit 1; fi
if [[ "$name" == python3 && "${1:-}" == -m && "${2:-}" == unittest && "${4:-}" == tests.hardware.* ]]; then
  printf '%s\n' "$PVISOR_VM_PROBE_BINARY" "$PVISOR_VM_PROBE_REPORT" "${PVISOR_CASE_VM_LIBRARY_DIR:-}" > "$PVISOR_TASK_TEST_LOG.probe-env"
  if [[ "${PVISOR_TASK_TEST_BAD_RUN:-0}" == 1 ]]; then exit 1; fi
fi
SH
chmod +x "$work/bin/tool"
for name in cargo rustc rustup uv uvx python3 node codesign actionlint zig unshare fusermount3 timeout go bats; do ln -s tool "$work/bin/$name"; done
export PATH="$work/bin:$PATH"
command_line() { printf '%q\t' "$@"; printf '\n'; }
run() { : > "$PVISOR_TASK_TEST_LOG"; local task="$1"; shift; mise -C "$work" run "$task" -- "$@" > "$work/stdout" 2> "$work/stderr"; }
expect_failure() { if run "$@"; then echo "expected failure: $*" >&2; exit 1; fi; }
check_line() { command_line "$@" > "$work/expected-line"; grep -Fxq -- "$(cat "$work/expected-line")" "$PVISOR_TASK_TEST_LOG" || { cat "$PVISOR_TASK_TEST_LOG" >&2; return 1; }; }
signature_lines() {
  command_line cargo nextest list --locked --message-format json "$@"
  for package in pvisor-vm pvisor pvisor-cli; do
    command_line codesign --force --sign - --entitlements "$work/crates/pvisor/macos-hypervisor.entitlements" "$work/tests with spaces/$package"$'\nfixture'
  done
}

# Assert routing decisions and side effects, not the inventory or every recipe.
run setup
check_line uv sync --locked --only-group dev
case "$(uname -m)" in
  arm64|aarch64) check_line rustup target add aarch64-unknown-linux-musl ;;
  x86_64) check_line rustup target add x86_64-unknown-linux-musl ;;
esac
run setup python
command_line uv sync --locked --only-group dev > "$work/expected"
diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
expect_failure setup unknown
test ! -s "$PVISOR_TASK_TEST_LOG"
expect_failure setup python extra
test ! -s "$PVISOR_TASK_TEST_LOG"
PVISOR_TASK_TEST_BAD_SYNC=1 expect_failure setup
test "$(wc -l < "$PVISOR_TASK_TEST_LOG")" -eq 1
CARGO_TARGET_DIR='relative with spaces' run build debug shim
check_line python3 scripts/packaging/stage_wheel_binaries.py --profile dev --target-dir "$work/relative with spaces" --shim
: > "$PVISOR_TASK_TEST_LOG"
(cd "$work/tests" && mise run check -- -p pvisor-core) > "$work/stdout" 2> "$work/stderr"
check_line cargo check --locked -p pvisor-core

run test core capture pvisor-overlay-core
command_line cargo nextest run --locked -p pvisor-core -p pvisor-gateway -p pvisor-overlay-core > "$work/expected"
diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
for package in pvisor-vm pvisor cli pvisor-cli; do
  actual=$package
  if [[ "$package" == cli ]]; then actual=pvisor-cli; fi
  run test "$package"
  if [[ "$(uname -s)" == Darwin ]]; then
    signature_lines -p "$actual" > "$work/expected"
  else
    : > "$work/expected"
  fi
  command_line cargo nextest run --locked -p "$actual" >> "$work/expected"
  diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
done
run test-rust
check_line cargo nextest run --locked --workspace
if [[ "$(uname -s)" == Darwin ]]; then check_line cargo nextest list --locked --message-format json --workspace; fi
run sign-vm-tests --workspace
signature_lines --workspace > "$work/expected"
diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
PVISOR_TASK_TEST_BAD_LIST=1 expect_failure sign-vm-tests --workspace
command_line cargo nextest list --locked --message-format json --workspace > "$work/expected"
diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
PVISOR_TASK_TEST_BAD_CODESIGN=1 expect_failure sign-vm-tests --workspace
test "$(wc -l < "$PVISOR_TASK_TEST_LOG")" -eq 2
run test-rust capture -- --test llm_fixtures
check_line cargo nextest run --locked -p pvisor-gateway --test llm_fixtures
run test cli -- --features pvisor-cli/gateway --cargo-profile dev-debug --test run_config_cli --target-dir "$work/test artifacts" --no-capture --test-threads 1
if [[ "$(uname -s)" == Darwin ]]; then
  signature_lines -p pvisor-cli --features pvisor-cli/gateway --cargo-profile dev-debug --test run_config_cli --target-dir "$work/test artifacts" > "$work/expected"
else
  : > "$work/expected"
fi
command_line cargo nextest run --locked -p pvisor-cli --features pvisor-cli/gateway --cargo-profile dev-debug --test run_config_cli --target-dir "$work/test artifacts" --no-capture --test-threads 1 >> "$work/expected"
diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
run test cli -- --features=pvisor-cli/gateway --release --no-default-features --offline
if [[ "$(uname -s)" == Darwin ]]; then
  check_line cargo nextest list --locked --message-format json -p pvisor-cli --features=pvisor-cli/gateway --release --no-default-features --offline
  PVISOR_TASK_TEST_BAD_LIST=1 expect_failure test cli -- --features pvisor-cli/gateway
  test "$(wc -l < "$PVISOR_TASK_TEST_LOG")" -eq 1
fi
run test shim -- --features vm
check_line cargo nextest run --locked -p pvisor-shim --features vm
# The native runner owns build/discovery. Only preflight refusal and stale
# evidence cleanup need a routing fixture; success is tested on a real host.
evidence="$CARGO_TARGET_DIR/nextest/rootless"
mkdir -p "$evidence"
printf stale > "$evidence/junit.xml"
printf stale > "$evidence/list.json"
PVISOR_TASK_TEST_BAD_ISOLATION=1 expect_failure test-isolation
test ! -e "$evidence/junit.xml"
test ! -e "$evidence/list.json"
if grep -Fq $'cargo\tmetadata\t' "$PVISOR_TASK_TEST_LOG" || grep -Fq $'cargo\tnextest\trun\t' "$PVISOR_TASK_TEST_LOG"; then
  echo 'failed rootless preflight reached test discovery or execution' >&2
  exit 1
fi
# Hardware selection must fail before Cargo when its inputs are unusable.
for firmware in "" relative.bin "$work/missing-firmware"; do
  PVISOR_CASE_VM_FIRMWARE_PATH="$firmware" expect_failure test-vm-lifecycle
  test ! -s "$PVISOR_TASK_TEST_LOG"
done
firmware="$work/local firmware.bin"
touch "$firmware"
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) abort_test=ready_abort_kvm ;;
  Darwin-arm64) abort_test=ready_abort_hvf ;;
  *) abort_test= ;;
esac
if [[ -n "$abort_test" ]]; then
  PVISOR_CASE_VM_FIRMWARE_PATH="$firmware" run test-vm-lifecycle
  check_line cargo nextest run --locked -p pvisor-vm --test ready_before_execution --test "$abort_test" --profile vm-lifecycle --run-ignored only --no-tests fail --test-threads 1
  PVISOR_CASE_VM_FIRMWARE_PATH="$firmware" PVISOR_TASK_TEST_BAD_NATIVE=1 expect_failure test-vm-lifecycle
  PVISOR_CASE_VM_FIRMWARE_PATH="$firmware" expect_failure test-vm-lifecycle extra-filter
  test ! -s "$PVISOR_TASK_TEST_LOG"
else
  PVISOR_CASE_VM_FIRMWARE_PATH="$firmware" expect_failure test-vm-lifecycle
fi
# One representative native usage route checks literal paths and failure
# propagation; snapshot semantics belong to the real hardware TestCases.
: > "$PVISOR_TASK_TEST_LOG"
PVISOR_CASE_VM_LIBRARY_DIR="$work/firmware with spaces" mise -C "$work" run --skip-deps test-environment-snapshot --report "$work/custom report.json" > "$work/stdout" 2> "$work/stderr"
diff -u <(printf '%s\n' "$CARGO_TARGET_DIR/debug/examples/vm_environment_snapshot" "$work/custom report.json" "$work/firmware with spaces") "$PVISOR_TASK_TEST_LOG.probe-env"
if PVISOR_TASK_TEST_BAD_RUN=1 mise -C "$work" run --skip-deps test-environment-snapshot > "$work/stdout" 2> "$work/stderr"; then
  echo 'hardware unittest failure was not propagated' >&2
  exit 1
fi
if [[ "$(uname -s)-$(uname -m)" == Darwin-arm64 ]]; then
  # Build, sign and verify must each stop execution on failure.
  PVISOR_TASK_TEST_BAD_BUILD=1 expect_failure test-environment-snapshot
  test "$(wc -l < "$PVISOR_TASK_TEST_LOG")" -eq 1
  PVISOR_TASK_TEST_BAD_CODESIGN=1 expect_failure test-environment-snapshot
  test "$(wc -l < "$PVISOR_TASK_TEST_LOG")" -eq 2
  PVISOR_TASK_TEST_BAD_VERIFY=1 expect_failure test-environment-snapshot
  test "$(wc -l < "$PVISOR_TASK_TEST_LOG")" -eq 3
else
  expect_failure test-environment-snapshot
  test ! -s "$PVISOR_TASK_TEST_LOG"
fi

expect_failure build release unknown
test ! -s "$PVISOR_TASK_TEST_LOG"
expect_failure build unknown shim
test ! -s "$PVISOR_TASK_TEST_LOG"
PVISOR_TASK_TEST_BAD_BUILD=1 expect_failure build debug shim-vm
test "$(wc -l < "$PVISOR_TASK_TEST_LOG")" -eq 1
if [[ "$(uname -s)" == Linux ]]; then
  mkdir -p "$CARGO_TARGET_DIR/release"
  printf retained > "$CARGO_TARGET_DIR/release/pvisor-memory-pool"
  run install-cli
  installed_binaries=(pvisor pvisor-replay pvisor-daemon)
  for binary in "${installed_binaries[@]}"; do test -x "$CARGO_INSTALL_ROOT/bin/$binary"; done
  test "$(find "$CARGO_INSTALL_ROOT/bin" -type f | wc -l)" -eq "${#installed_binaries[@]}"
  test ! -e "$CARGO_INSTALL_ROOT/bin/pvisor-memory-pool"
  test "$(cat "$CARGO_TARGET_DIR/release/pvisor-memory-pool")" == retained
fi

run ci check 'test core'
{ command_line cargo check --locked -p pvisor-cli; command_line cargo nextest run --locked -p pvisor-core; } > "$work/expected"
diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
expect_failure ci "check;touch $work/injected" build
test ! -e "$work/injected"
test ! -s "$PVISOR_TASK_TEST_LOG"
ln -s "$MISE_PROJECT_ROOT/benchmark" "$work/benchmark"
for recipe in fw cases benchmark doctor; do
  case "$recipe" in
    cases) expect_failure cases --suite unknown ;;
    benchmark) expect_failure benchmark smoke unknown ;;
    *) expect_failure "$recipe" unknown ;;
  esac
  if [[ "$recipe" == benchmark ]]; then
    command_line cargo run --release --locked --manifest-path "$work/benchmark/criterion/Cargo.toml" --bin pvisor-benchmarks -- smoke unknown > "$work/expected"
    diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
    grep -Fq "unexpected argument 'unknown'" "$work/stderr"
  else
    test ! -s "$PVISOR_TASK_TEST_LOG"
  fi
done
run benchmark startup/timing --row 'pvisor vm'
command_line cargo run --release --locked --manifest-path "$work/benchmark/criterion/Cargo.toml" --bin pvisor-benchmarks -- startup/timing --row 'pvisor vm' > "$work/expected"
diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
# Spaces, shell metacharacters and the Criterion separator remain literal argv.
benchmark_binary="$work/already built/pvisor;\$(touch $work/injected)"
run benchmark smoke --binary "$benchmark_binary" --output "$work/evidence with spaces" -- --baseline main
command_line cargo run --release --locked --manifest-path "$work/benchmark/criterion/Cargo.toml" --bin pvisor-benchmarks -- smoke --binary "$benchmark_binary" --output "$work/evidence with spaces" -- --baseline main > "$work/expected"
diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
test ! -e "$work/injected"
PVISOR_TASK_TEST_BAD_BUILD=1 expect_failure benchmark filesystem/tools --row native-read
command_line cargo run --release --locked --manifest-path "$work/benchmark/criterion/Cargo.toml" --bin pvisor-benchmarks -- filesystem/tools --row native-read > "$work/expected"
diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
# No default product build or custom Markdown selection parser.
run benchmark smoke
command_line cargo run --release --locked --manifest-path "$work/benchmark/criterion/Cargo.toml" --bin pvisor-benchmarks -- smoke > "$work/expected"
diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
run benchmark-compare "$work/results with spaces" --baseline main --candidate candidate --threshold 15 --output "$work/comparison with spaces"
command_line cargo run --release --locked --manifest-path "$work/benchmark/criterion/Cargo.toml" --bin benchmark-compare -- "$work/results with spaces" --baseline main --candidate candidate --threshold 15 --output "$work/comparison with spaces" > "$work/expected"
diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
PVISOR_TASK_TEST_BAD_BUILD=1 expect_failure benchmark-compare "$work/results" --output "$work/comparison"
command_line cargo run --release --locked --manifest-path "$work/benchmark/criterion/Cargo.toml" --bin benchmark-compare -- "$work/results" --output "$work/comparison" > "$work/expected"
diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
PVISOR_TASK_TEST_BAD_DOCS_PREFLIGHT=1 PVISOR_TASK_TEST_BAD_BUILD=1 run docs-serve en -a 127.0.0.1:3002
check_line uv run --locked --only-group dev zensical serve -f docs/zensical.en.toml -a 127.0.0.1:3002
grep -Fq 'continuing live preview' "$work/stderr"
mkdir -p "$work/docs/site"
printf previous > "$work/docs/site/previous.html"
PVISOR_TASK_TEST_BAD_DOCS_PREFLIGHT=1 expect_failure docs-build
test "$(cat "$work/docs/site/previous.html")" == previous
if grep -Fq $'zensical\tbuild\t' "$PVISOR_TASK_TEST_LOG"; then
  echo 'failed strict docs preflight reached the site builder' >&2
  exit 1
fi
selection="S-DOC-001,\$(touch $work/injected)"
expect_failure cases --case "$selection" --keep
test ! -e "$work/injected"
run cases --suite use --case S-USE-001,S-USE-002 --case=S-USE-003 --output "$work/report with spaces.json" --keep
check_line scrut test --renderer json "$work/tests/spec/session/S-USE-001.md" "$work/tests/spec/session/S-USE-002.md" "$work/tests/spec/session/S-USE-003.md"
PVISOR_TASK_TEST_BAD_RUN=1 expect_failure cases --suite use --case S-USE-001
check_line scrut test --renderer json "$work/tests/spec/session/S-USE-001.md"
expect_failure cases --suite use --case S-USE-001 --require-reviewed
grep -Fq 'refusing --require-reviewed' "$work/stderr"
expect_failure cases --suite use --case
if grep -Fq $'scrut\ttest\t' "$PVISOR_TASK_TEST_LOG"; then echo 'invalid selection reached Scrut' >&2; exit 1; fi

# A new feature-local case is discovered without changing task or CI ID lists.
printf '# routing fixture\n' > "$work/tests/spec/config/S-CONFIG-999.md"
run cases-feature config --list
grep -Fq 'S-CONFIG-999.md' "$work/stdout"
test ! -s "$PVISOR_TASK_TEST_LOG"
run cases-feature config --case S-CONFIG-999 --subject-bin "$CARGO_TARGET_DIR/release/pvisor"
check_line scrut test --renderer json "$work/tests/spec/config/S-CONFIG-999.md"
test "$(cat "$PVISOR_TASK_TEST_LOG.strict")" == 0
test -f "$CARGO_TARGET_DIR/cases-config.json"
run cases-feature-strict config --case S-CONFIG-999 --subject-bin "$CARGO_TARGET_DIR/release/pvisor"
test "$(cat "$PVISOR_TASK_TEST_LOG.strict")" == 1
PVISOR_TASK_TEST_BAD_RUN=1 expect_failure cases-feature config --case S-CONFIG-999
expect_failure cases-feature config --feature session
expect_failure cases-feature config --suite use
expect_failure cases-feature
expect_failure cases-feature unknown --list
expect_failure test-feature-native docker-cli unknown
expect_failure test-feature-native unknown
expect_failure test-feature-contracts docker-compose bats
expect_failure test-feature-contracts docker-cli go-2
test ! -s "$PVISOR_TASK_TEST_LOG"
run test-feature-native docker-cli
check_line cargo nextest run --locked -p pvisor-cli --test docker_cli --test docker_p0_cli --test docker_staged_cli --test docker_staged_native -E 'not (test(compose))' --no-tests fail
run test-feature-native docker-compose
check_line cargo nextest run --locked -p pvisor-cli --test compose_cli --test docker_cli -E 'binary(=compose_cli) | test(compose)' --no-tests fail
PVISOR_TASK_TEST_BAD_NATIVE=1 expect_failure test-feature-native docker-cli
PVISOR_TASK_TEST_BAD_NATIVE=1 expect_failure test-feature-native docker-compose

export PVISOR_BINARY="$CARGO_TARGET_DIR/release/pvisor"
export CONTRACT_LOGS="$work/bats logs"
run test-feature-contracts docker-cli bats
check_line bats --tap --print-output-on-failure tests/spec/docker-cli/regression/system
PVISOR_TASK_TEST_BAD_RUN=1 expect_failure test-feature-contracts docker-cli bats
for shard in 0 1; do
  CONTRACT_LOGS="$work/go-$shard" run test-feature-contracts docker-cli "go-$shard"
  check_line go test -list '^Test' tests/spec/docker-cli/regression/e2e/*.go
done
# Every discovered name goes to exactly one shard, including a newly added test.
cat "$work/go-0/selected.txt" "$work/go-1/selected.txt" | LC_ALL=C sort > "$work/selected"
printf '%s\n' Test4 TestAlpha TestBeta TestNewCase > "$work/expected"
diff -u "$work/expected" "$work/selected"
PVISOR_TASK_TEST_BAD_RUN=1 expect_failure test-feature-contracts docker-cli go-0

run accept-wheel "$work/wheel with spaces.whl" "$work/evidence with spaces.json"
check_line python3 scripts/packaging/verify_wheel.py "$work/wheel with spaces.whl" --install-smoke --acceptance-log "$work/evidence with spaces.json"
expect_failure accept-wheel only-one-argument
test ! -s "$PVISOR_TASK_TEST_LOG"

run wheel
test -f "$work/dist/pvisor_sandbox-a.whl"
test -f "$work/dist/pvisor_sandbox-b.whl"
test -f "$work/dist/pvisor-firmware-source-linux-x86_64.tar.gz"
grep -Fq 'scripts/packaging/verify_wheel.py' "$PVISOR_TASK_TEST_LOG"
grep -Fq $'uv\trun\t--locked\t--only-group\tdev\ttwine\tcheck\t' "$PVISOR_TASK_TEST_LOG"
test "$(find "$CARGO_TARGET_DIR" -maxdepth 1 -name 'wheel.*' | wc -l)" -eq 0
rm -rf "$work/dist"
PVISOR_WHEEL_PORTABLE=1 run wheel debug
grep -Fq $'uv\trun\t--locked\t--only-group\tdev\tcibuildwheel\t' "$PVISOR_TASK_TEST_LOG"
rm -rf "$work/dist"
mkdir "$work/dist"
printf previous > "$work/dist/existing.whl"
PVISOR_TASK_TEST_BAD_WHEEL=1 expect_failure wheel
test "$(cat "$work/dist/existing.whl")" == previous
test "$(find "$work/dist" -type f | wc -l)" -eq 1
test "$(find "$CARGO_TARGET_DIR" -maxdepth 1 -name 'wheel.*' | wc -l)" -eq 0

# The benchmark link above points at real source; cleanup fixtures stay private.
rm "$work/benchmark"
mkdir -p "$work/.venv" "$CARGO_TARGET_DIR/pvisor-examples" "$work/benchmark/pvisor/.data" "$work/build" "$work/docs/site" "$CARGO_TARGET_DIR/fw"
for path in "$work/.venv/state" "$CARGO_TARGET_DIR/pvisor-examples/evidence" "$work/benchmark/pvisor/.data/report.json" "$work/build/output" "$work/docs/site/output" "$CARGO_TARGET_DIR/fw/kernel"; do touch "$path"; done
run clean
for path in "$work/.venv/state" "$CARGO_TARGET_DIR/pvisor-examples/evidence" "$work/benchmark/pvisor/.data/report.json"; do test -f "$path"; done
for path in "$work/build" "$work/dist" "$work/docs/site" "$CARGO_TARGET_DIR/fw"; do test ! -e "$path"; done
echo 'PASS: mise argument ownership, prerequisite refusal, signing, publication and cleanup'
