# PolicyVisor development tasks. Run `just` to list them.
set positional-arguments

repo := justfile_directory()
target_dir := absolute_path(env("CARGO_TARGET_DIR", repo / "target"))
python_paths := "pvisor tests examples"

default:
    @just --list --unsorted

# Build pvisor (debug or release), including macOS Hypervisor signing.
build profile="debug":
    #!/usr/bin/env bash
    set -euo pipefail
    case "$1" in
      debug) cargo_profile=dev ;;
      release) cargo_profile=release ;;
      *) echo "expected debug or release, got: $1" >&2; exit 2 ;;
    esac
    cargo build --locked --profile "$cargo_profile" --target-dir "{{ target_dir }}" -p persisting-pvisor --bin pvisor
    binary="{{ target_dir }}/$1/pvisor"
    test -x "$binary"
    if [[ "$(uname -s)" == Darwin ]]; then
      codesign --force --sign - --entitlements "{{ repo }}/crates/persisting-pvisor/macos-hypervisor.entitlements" "$binary"
      codesign --verify --strict "$binary"
      codesign -d --entitlements :- "$binary" 2>&1 | grep -q com.apple.security.hypervisor
    fi

# Install the signed release binary in CARGO_INSTALL_ROOT or ~/.cargo.
install-cli: (build "release")
    #!/usr/bin/env bash
    set -euo pipefail
    install_root="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"
    mkdir -p "$install_root/bin"
    install -m 755 "{{ target_dir }}/release/pvisor" "$install_root/bin/pvisor"

# Build and verify a fresh wheel before placing it in dist/ (release or debug).
wheel profile="release":
    #!/usr/bin/env bash
    set -euo pipefail
    case "$1" in
      release) cargo_profile=release ;;
      debug) cargo_profile=dev ;;
      *) echo "expected debug or release, got: $1" >&2; exit 2 ;;
    esac
    mkdir -p "{{ target_dir }}"
    staging=$(mktemp -d "{{ target_dir }}/wheel.XXXXXX")
    trap 'rm -rf "$staging"' EXIT
    uv build --wheel --out-dir "$staging" --config-setting "cargo-profile=$cargo_profile"
    for wheel in "$staging"/pvisor-*.whl; do
      python3 scripts/packaging/verify_wheel.py "$wheel" --install-smoke
      mkdir -p dist
      mv "$wheel" dist/
    done

# Check the product and its dependencies without producing a binary.
check:
    cargo check --locked -p persisting-pvisor

# Format source files; use fmt-check for a read-only check.
fmt: fmt-rust fmt-py

fmt-rust *args:
    cargo fmt --all -- "$@"

fmt-py *args:
    uvx ruff format {{ python_paths }} "$@"

fmt-check: (fmt-rust "--check") (fmt-py "--check")

lint: lint-rust lint-py

lint-rust:
    cargo clippy --workspace --all-targets --locked -- -D warnings

lint-py:
    uvx ruff check pvisor

# Run all Rust and Python tests, or only the selected Rust packages.
test *packages:
    #!/usr/bin/env bash
    set -euo pipefail
    just test-rust "$@"
    if [[ $# -eq 0 ]]; then just test-py; fi

# Debug nextest; accepts Cargo names and pvisor/control/agentctl/capture aliases.
test-rust *packages:
    #!/usr/bin/env bash
    set -euo pipefail
    args=()
    for package in "$@"; do
      case "$package" in
        pvisor) package=persisting-pvisor ;;
        control|agentctl) package=persisting-control ;;
        capture) package=persisting-gateway ;;
      esac
      args+=(-p "$package")
    done
    if [[ $# -eq 0 ]]; then args+=(--workspace); fi
    cargo nextest run --locked "${args[@]}"

# Python tests; append pytest options such as -v or -k packaging.
test-py *args:
    uv run --extra dev pytest tests/ -q "$@"

# Strict Linux rootless/FUSE regression: never skip missing user namespaces.
test-isolation:
    env -u PERSISTING_TEST_ALLOW_NO_USERNS cargo nextest run --locked -p persisting-pvisor --test rootless_local -- --nocapture

# Build the debug CLI and check its main command surfaces.
smoke: build
    #!/usr/bin/env bash
    set -euo pipefail
    for command in run status review; do
      "{{ target_dir }}/debug/pvisor" "$command" --help >/dev/null
    done

# Run all examples, or pass scenario directory names to select a subset.
examples *scenarios: (build "release")
    PVISOR_BIN="{{ target_dir }}/release/pvisor" bash examples/pvisor/test.sh "$@"

# Run documented cases; accepts the case runner's options.
cases *args: (build "release")
    python3 scripts/run-pvisor-cases.py --pvisor "{{ target_dir }}/release/pvisor" --report target/pvisor-case-report.md "$@"

# Measure process startup and Run Bundle access (smoke or nightly).
benchmark suite="smoke" output="target/pvisor-benchmark/current" build_dir="target/pvisor-benchmark-build":
    bash benchmark/pvisor/run.sh run --suite "$1" --output "$2" --target-dir "$3"

# Compare reports from the same host; an empty baseline is allowed.
benchmark-compare candidate baseline="" output="target/pvisor-benchmark/comparison" threshold="15":
    bash benchmark/pvisor/run.sh compare --candidate "$1" --baseline "$2" --output "$3" --regression-threshold "$4"

test-benchmark:
    PYTHONDONTWRITEBYTECODE=1 python3 benchmark/pvisor/test_bench.py

# Build both languages with the same pinned tool as CI, then validate links.
docs-build:
    uv run --no-project --with zensical==0.0.61 python scripts/build-docs.py
    python3 scripts/check-docs.py

# Watch and serve documentation on localhost:3000; accepts --port and --host.
docs-serve *args: docs-build
    uv run --no-project --with zensical==0.0.61 python scripts/serve-docs.py --directory docs/site --watch "$@"

# Read-only local checks, followed by tests and a debug build.
ci: fmt-check lint test build

# Remove build outputs; keep development environments and local Run records.
clean:
    cargo clean
    rm -rf build dist
