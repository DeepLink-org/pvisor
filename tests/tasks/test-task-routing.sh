#!/usr/bin/env bash
set -Eeuo pipefail
if [[ $# -gt 0 ]]; then echo "too many arguments for test-task-routing" >&2; exit 2; fi
work=$(mktemp -d)
work=$(cd "$work" && pwd -P)
trap 'echo "FAIL: $BASH_COMMAND" >&2; cat "$work/stdout" "$work/stderr" >&2 2>/dev/null || true' ERR
trap 'rm -rf "$work"' EXIT
test "$UV_PYTHON" == "$(mise where python)/bin/python3"
# A fresh tool store must allow inspection before installation.
MISE_DATA_DIR="$work/uninstalled-tools" mise -C "$MISE_PROJECT_ROOT" task fmt > "$work/task-info"
grep -Fq 'Task: fmt' "$work/task-info"
MISE_DATA_DIR="$work/uninstalled-tools" mise -C "$MISE_PROJECT_ROOT" env --json > "$work/env.json"
jq -e '.UV_PYTHON == "python3"' "$work/env.json" > /dev/null
# Run a release check with only Python present; nested task startup must not
# install Rust, Zig or the Cargo plugins in an explicitly provisioned CI job.
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
export PVISOR_TASK_TEST_LOG="$work/commands" CARGO_TARGET_DIR="$work/target with spaces" CARGO_INSTALL_ROOT="$work/install"
export PVISOR_TASK_TEST_PYTHON="$(python3 -c 'import sys; print(sys.executable)')"
mkdir "$work/bin" "$work/tests"
mkdir -p "$work/scripts/tasks" "$work/tests/tasks"
cp "$MISE_PROJECT_ROOT"/scripts/tasks/*.sh "$work/scripts/tasks/"
cp "$MISE_PROJECT_ROOT"/tests/tasks/*.sh "$work/tests/tasks/"
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
if [[ "$name" == python3 && "$1" == scripts/check-docs.py && "${PVISOR_TASK_TEST_BAD_DOCS_PREFLIGHT:-0}" == 1 ]]; then exit 1; fi
if [[ "$name" == python3 && "$1" == scripts/packaging/stage_wheel_binaries.py ]]; then
  if [[ "${PVISOR_TASK_TEST_BAD_BUILD:-0}" == 1 ]]; then exit 1; fi
  profile=debug
  for ((i=0; i<$#; i++)); do
    if [[ "${args[$i]}" == --profile ]]; then profile=${args[$((i+1))]}; fi
  done
  if [[ "$profile" == dev ]]; then profile=debug; fi
  names=(pvisor pvisor-tui pvisor-replay pvisor-daemon)
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
if [[ "$name" == cargo && "$1" == run && "${PVISOR_TASK_TEST_BAD_RUN:-0}" == 1 ]]; then exit 1; fi
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
      touch "$staging/pvisor-a.whl" "$staging/pvisor-b.whl" "$staging/pvisor-firmware-source-linux-x86_64.tar.gz"
    fi
  done
fi
if [[ "$name" == python3 && "$1" == scripts/packaging/verify_wheel.py && "${PVISOR_TASK_TEST_BAD_WHEEL:-0}" == 1 && "$2" == */pvisor-b.whl ]]; then exit 1; fi
SH
chmod +x "$work/bin/tool"
for name in cargo rustc rustup uv uvx python3 codesign actionlint zig; do ln -s tool "$work/bin/$name"; done
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

# Inspect the native task inventory, including the visibility contract.
mise -C "$work" tasks ls --json | jq -r '.[] | select(.hide == false) | .name' | LC_ALL=C sort > "$work/public"
printf '%s\n' default ci clean build check wheel fw fmt fmt-rust fmt-py lint lint-rust lint-py lint-workflows test test-rust test-py test-isolation smoke examples cases test-hvf-cold-restore test-vm-snapshot-state benchmark docs-build docs-serve check-core-budget update-core-budget check-release-version check-release-artifacts doctor setup | LC_ALL=C sort > "$work/expected"
diff -u "$work/expected" "$work/public"

run doctor dev
check_line cargo nextest --version
run fw build --offline
check_line uv run --locked --only-group dev python scripts/packaging/firmware.py --target-dir "$CARGO_TARGET_DIR" --offline
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
run test-semspec --test-threads 1
{ command_line cargo nextest run --manifest-path tools/semspec/Cargo.toml --target-dir "$CARGO_TARGET_DIR/semspec" --locked --test-threads 1
  command_line python3 -m unittest discover -s tools/semspec -p 'test_*.py'; } > "$work/expected"
diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
run lint-semspec
check_line cargo clippy --manifest-path tools/semspec/Cargo.toml --target-dir "$CARGO_TARGET_DIR/semspec" --all-targets --locked -- -D warnings
run test shim -- --features vm
check_line cargo nextest run --locked -p pvisor-shim --features vm
run check -p pvisor-shim --target x86_64-unknown-linux-musl
check_line cargo check --locked -p pvisor-shim --target x86_64-unknown-linux-musl
run lint-rust -p pvisor-shim --features vm
check_line cargo clippy --all-targets --locked -p pvisor-shim --features vm -- -D warnings
run test-py
check_line uv run --locked --only-group dev python -m unittest discover -s tests
check_line uv run --locked --only-group dev python -m unittest discover -s benchmark/support
for suite in tests filesystem memory; do
  check_line uv run --locked --only-group dev python -m unittest discover -s "benchmark/$suite"
done
for flags in selected leading filter; do
  case "$flags" in
    selected) run test-py tests/test_docs.py -k reference -v; check_line uv run --locked --only-group dev python -m unittest tests/test_docs.py -k reference -v ;;
    leading) run test-py -q tests/test_docs.py; check_line uv run --locked --only-group dev python -m unittest -q tests/test_docs.py ;;
    filter) run test-py -k packaging; check_line uv run --locked --only-group dev python -m unittest discover -s tests -k packaging; check_line uv run --locked --only-group dev python -m unittest discover -s benchmark/support -k packaging ;;
  esac
done
for recipe in test-hvf-cold-restore test-vm-snapshot-state test-linux-cold-restore test-environment-snapshot; do
  if [[ "$(uname -s)-$(uname -m)" != Darwin-arm64 ]]; then
    expect_failure "$recipe"
    test ! -s "$PVISOR_TASK_TEST_LOG"
    continue
  fi
  package=pvisor-vm
  case "$recipe" in
    test-hvf-cold-restore) example=hvf_cold_restore_case ;;
    test-vm-snapshot-state) example=threaded_cold_restore_case ;;
    test-linux-cold-restore) example=vm_linux_cold_restore; package=pvisor ;;
    test-environment-snapshot) example=vm_environment_snapshot; package=pvisor ;;
  esac
  run "$recipe"
  if [[ "$recipe" == test-hvf-cold-restore ]]; then
    check_line cargo build --locked -p "$package" --example "$example" --target-dir "$CARGO_TARGET_DIR"
  else
    check_line cargo build --locked -p "$package" --example "$example" --target-dir "$CARGO_TARGET_DIR" --offline
  fi
  binary="$CARGO_TARGET_DIR/debug/examples/$example"
  check_line codesign --force --sign - --entitlements "$work/crates/pvisor/macos-hypervisor.entitlements" "$binary"
  check_line codesign --verify --strict "$binary"
  if [[ "$package" == pvisor ]]; then
    check_line rustc --edition 2024 --target aarch64-unknown-linux-musl -C linker=rust-lld -C opt-level=2 crates/pvisor-vm/src/probes/guest_linux.rs -o "$CARGO_TARGET_DIR/debug/probes/guest_linux"
    report=linux-machine
    if [[ "$recipe" == test-environment-snapshot ]]; then report=environment-linux; fi
    check_line python3 "scripts/check-${recipe#test-}.py" --binary "$binary" --guest-bin "$CARGO_TARGET_DIR/debug/probes/guest_linux" --firmware-dir "$CARGO_TARGET_DIR/release" --report "$CARGO_TARGET_DIR/vm-validation/$report.json"
    PVISOR_CASE_VM_LIBRARY_DIR="$work/firmware with spaces" run "$recipe" --report "$work/custom report.json"
    check_line python3 "scripts/check-${recipe#test-}.py" --binary "$binary" --guest-bin "$CARGO_TARGET_DIR/debug/probes/guest_linux" --firmware-dir "$work/firmware with spaces" --report "$CARGO_TARGET_DIR/vm-validation/$report.json" --report "$work/custom report.json"
  else
    check_line python3 "scripts/check-${recipe#test-}.py" --binary "$binary"
  fi
  PVISOR_TASK_TEST_BAD_BUILD=1 expect_failure "$recipe"
  test "$(wc -l < "$PVISOR_TASK_TEST_LOG")" -eq 1
  PVISOR_TASK_TEST_BAD_CODESIGN=1 expect_failure "$recipe"
  test "$(wc -l < "$PVISOR_TASK_TEST_LOG")" -eq 2
  PVISOR_TASK_TEST_BAD_VERIFY=1 expect_failure "$recipe"
  test "$(wc -l < "$PVISOR_TASK_TEST_LOG")" -eq 3
done

for profile in debug release performance; do
  run build "$profile"
  cargo_profile=$profile
  if [[ "$profile" == debug ]]; then cargo_profile=dev; fi
  check_line python3 scripts/packaging/stage_wheel_binaries.py --profile "$cargo_profile" --target-dir "$CARGO_TARGET_DIR" --all
  for binary in pvisor pvisor-tui pvisor-replay pvisor-daemon; do test -x "$CARGO_TARGET_DIR/$profile/$binary"; done
  if [[ "$(uname -s)" == Linux ]]; then
    for binary in pvisor-daemon pvisor-guest containerd-shim-pvisor-v2; do test -x "$CARGO_TARGET_DIR/$profile/$binary"; done
  fi
  run build "$profile" product
  check_line python3 scripts/packaging/stage_wheel_binaries.py --profile "$cargo_profile" --target-dir "$CARGO_TARGET_DIR"
  test ! -e "$CARGO_TARGET_DIR/$profile/pvisor-memory-pool"
  for component in daemon shim shim-vm; do
    run build "$profile" "$component"
    check_line python3 scripts/packaging/stage_wheel_binaries.py --profile "$cargo_profile" --target-dir "$CARGO_TARGET_DIR" "--$component"
    expected_commands=1
    if [[ "$component" == daemon && "$(uname -s)" == Darwin ]]; then expected_commands=3; fi
    test "$(wc -l < "$PVISOR_TASK_TEST_LOG")" -eq "$expected_commands"
    binary=containerd-shim-pvisor-v2
    if [[ "$component" == daemon ]]; then binary=pvisor-daemon; fi
    test -x "$CARGO_TARGET_DIR/$profile/$binary"
  done
done
expect_failure build release unknown
test ! -s "$PVISOR_TASK_TEST_LOG"
expect_failure build unknown shim
test ! -s "$PVISOR_TASK_TEST_LOG"
PVISOR_TASK_TEST_BAD_BUILD=1 expect_failure build debug shim-vm
test "$(wc -l < "$PVISOR_TASK_TEST_LOG")" -eq 1
if [[ "$(uname -s)" == Linux ]]; then
  printf retained > "$CARGO_TARGET_DIR/release/pvisor-memory-pool"
  run install-cli
  for binary in pvisor pvisor-tui pvisor-replay pvisor-daemon; do test -x "$CARGO_INSTALL_ROOT/bin/$binary"; done
  test "$(find "$CARGO_INSTALL_ROOT/bin" -type f | wc -l)" -eq 4
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
    command_line python3 "$work/benchmark/run.py" --check smoke unknown > "$work/expected"
    diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
  else
    test ! -s "$PVISOR_TASK_TEST_LOG"
  fi
done
run benchmark startup/timing 'pvisor vm'
check_line python3 "$work/benchmark/run.py" --check startup/timing 'pvisor vm'
check_line python3 "$work/benchmark/run.py" startup/timing 'pvisor vm'
run benchmark filesystem/tools native-read
check_line python3 "$work/benchmark/run.py" filesystem/tools native-read
expect_failure benchmark filesystem/tools 'pvisor vm'
command_line python3 "$work/benchmark/run.py" --check filesystem/tools 'pvisor vm' > "$work/expected"
diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
grep -Fq 'available: native-metadata, native-read' "$work/stderr"
run benchmark smoke --binary /already-built/pvisor
{
  command_line python3 "$work/benchmark/run.py" --check smoke --binary /already-built/pvisor
  command_line cargo build --quiet --locked --manifest-path "$work/tools/semspec/Cargo.toml" --target-dir "$CARGO_TARGET_DIR/semspec"
  command_line python3 "$work/benchmark/run.py" smoke --binary /already-built/pvisor
} > "$work/expected"
diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
run benchmark smoke
check_line python3 "$work/benchmark/run.py" --binary "$CARGO_TARGET_DIR/pvisor-benchmark-build/release/pvisor" smoke
check_line python3 scripts/packaging/stage_wheel_binaries.py --profile release --target-dir "$CARGO_TARGET_DIR/pvisor-benchmark-build" --all
PVISOR_TASK_TEST_BAD_DOCS_PREFLIGHT=1 PVISOR_TASK_TEST_BAD_BUILD=1 run docs-serve en -a 127.0.0.1:3002
check_line uv run --locked --only-group dev zensical serve -f docs/zensical.en.toml -a 127.0.0.1:3002
grep -Fq 'continuing live preview' "$work/stderr"
PVISOR_TASK_TEST_BAD_DOCS_PREFLIGHT=1 expect_failure docs-build
{
  command_line python3 benchmark/docs.py check
  command_line python3 scripts/check-reference.py
  command_line python3 scripts/check-docs.py --preflight-build
} > "$work/expected"
diff -u "$work/expected" "$PVISOR_TASK_TEST_LOG"
selection="S-DOC-001,\$(touch $work/injected)"
run cases --case "$selection" --keep --require-reviewed
check_line cargo run --quiet --manifest-path tools/semspec/Cargo.toml --target-dir "$CARGO_TARGET_DIR/semspec" --locked -- run docs/src/zh/reference --domain DOC --subject-bin "$CARGO_TARGET_DIR/release/pvisor" --format json --output "$CARGO_TARGET_DIR/pvisor-case-report.json" --case "$selection" --keep --require-reviewed
test ! -e "$work/injected"
run cases --suite vm --case S-DOC-060 --keep
check_line cargo build --locked -p pvisor --release --example vm_control_case --target-dir "$CARGO_TARGET_DIR"
check_line cargo run --quiet --manifest-path tools/semspec/Cargo.toml --target-dir "$CARGO_TARGET_DIR/semspec" --locked -- run docs/src/zh/reference/cases-vm.md --domain DOC --subject-bin "$CARGO_TARGET_DIR/release/pvisor" --format json --output "$CARGO_TARGET_DIR/pvisor-vm-case-report.json" --case S-DOC-060 --keep
run cases --suite stage --case S-STAGE-001 --keep
check_line cargo run --quiet --manifest-path tools/semspec/Cargo.toml --target-dir "$CARGO_TARGET_DIR/semspec" --locked -- run docs/src/zh/cases/06-stage-apply.md --domain STAGE --subject-bin "$CARGO_TARGET_DIR/debug/pvisor" --case S-STAGE-001 --keep
run cases --suite use
run cases --suite use --case S-USE-001,S-USE-002 --case=S-USE-003 --output "$work/report with spaces.json" --keep --require-reviewed
check_line cargo run --quiet --manifest-path tools/semspec/Cargo.toml --target-dir "$CARGO_TARGET_DIR/semspec" --locked -- run docs/src/zh/cases --domain USE --require-pass --subject-bin "$CARGO_TARGET_DIR/release/pvisor" --format json --output "$work/report with spaces.json" --case S-USE-001,S-USE-002 --case S-USE-003 --keep --require-reviewed
PVISOR_TASK_TEST_BAD_RUN=1 expect_failure cases --suite use --case S-USE-001 --require-reviewed
check_line cargo run --quiet --manifest-path tools/semspec/Cargo.toml --target-dir "$CARGO_TARGET_DIR/semspec" --locked -- run docs/src/zh/cases --domain USE --require-pass --subject-bin "$CARGO_TARGET_DIR/release/pvisor" --format json --output "$CARGO_TARGET_DIR/pvisor-learning-report.json" --case S-USE-001 --require-reviewed
expect_failure cases --suite use --case
if grep -Fq $'cargo\trun\t' "$PVISOR_TASK_TEST_LOG"; then echo 'invalid selection reached semspec' >&2; exit 1; fi

run wheel
test -f "$work/dist/pvisor-a.whl"
test -f "$work/dist/pvisor-b.whl"
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

mkdir -p "$work/.venv" "$CARGO_TARGET_DIR/pvisor-examples" "$work/benchmark/pvisor/.data" "$work/build" "$work/docs/site" "$CARGO_TARGET_DIR/fw"
for path in "$work/.venv/state" "$CARGO_TARGET_DIR/pvisor-examples/evidence" "$work/benchmark/pvisor/.data/report.json" "$work/build/output" "$work/docs/site/output" "$CARGO_TARGET_DIR/fw/kernel"; do touch "$path"; done
run clean
for path in "$work/.venv/state" "$CARGO_TARGET_DIR/pvisor-examples/evidence" "$work/benchmark/pvisor/.data/report.json"; do test -f "$path"; done
for path in "$work/build" "$work/dist" "$work/docs/site" "$CARGO_TARGET_DIR/fw"; do test ! -e "$path"; done
echo 'PASS: mise routing, task visibility, argument ownership, wheel publication and cleanup'
