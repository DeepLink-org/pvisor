# PolicyVisor development tasks. Run `just` to list them.
set positional-arguments

repo := justfile_directory()
target_dir := absolute_path(env("CARGO_TARGET_DIR", repo / "target"))
python_paths := "pvisor tests examples conftest.py"

default:
    @just --list --unsorted

# Build pvisor (debug, release or performance), including macOS Hypervisor signing.
build profile="debug":
    #!/usr/bin/env bash
    set -euo pipefail
    case "$1" in
      debug) cargo_profile=dev ;;
      release) cargo_profile=release ;;
      performance) cargo_profile=performance ;;
      *) echo "expected debug, release or performance, got: $1" >&2; exit 2 ;;
    esac
    python3 scripts/build-pvisor.py --profile "$cargo_profile" --target-dir "{{ target_dir }}"
    for name in pvisor pvisor-cache pvisor-tui pvisor-replay pvisor-memory-pool pvisor-daemon; do
      binary="{{ target_dir }}/$1/$name"
      test -x "$binary"
      if [[ "$(uname -s)" == Darwin && "$name" != pvisor-daemon ]]; then
        codesign --force --sign - --entitlements "{{ repo }}/crates/pvisor/macos-hypervisor.entitlements" "$binary"
        codesign --verify --strict "$binary"
      fi
    done

# Install the signed release binary in CARGO_INSTALL_ROOT or ~/.cargo.
install-cli: (build "release")
    #!/usr/bin/env bash
    set -euo pipefail
    install_root="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"
    mkdir -p "$install_root/bin"
    for binary in pvisor pvisor-cache pvisor-tui pvisor-replay pvisor-memory-pool pvisor-daemon; do
      install -m 755 "{{ target_dir }}/release/$binary" "$install_root/bin/$binary"
    done

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
    cargo check --locked -p pvisor

# Standalone daemon: no native executor, firmware or Hypervisor entitlement.
daemon-build:
    cargo build --locked -p pvisor-daemon --bin pvisor-daemon --no-default-features --target-dir "{{ target_dir }}"

# Install a standalone release daemon without building the native CLI.
daemon-install:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release --locked -p pvisor-daemon --bin pvisor-daemon --no-default-features --target-dir "{{ target_dir }}"
    install_root="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"
    mkdir -p "$install_root/bin"
    install -m 755 "{{ target_dir }}/release/pvisor-daemon" "$install_root/bin/pvisor-daemon"

# Conventional daemon contracts; does not enable the retired cluster feature.
test-daemon:
    just test pvisor-daemon

# Local node/cache/memory-pool components, plus the independently built daemon.
service-build:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --locked -p pvisor --bin pvisor --bin pvisor-cache --bin pvisor-memory-pool --features pvisor/gateway --target-dir "{{ target_dir }}"
    just daemon-build
    if [[ "$(uname -s)" == Darwin ]]; then
      for name in pvisor pvisor-memory-pool; do
        codesign --force --sign - --entitlements "{{ repo }}/crates/pvisor/macos-hypervisor.entitlements" "{{ target_dir }}/debug/$name"
      done
    fi


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

# Debug nextest; accepts Cargo names and pvisor/control/agentctl/capture/shim aliases.
test-rust *packages:
    #!/usr/bin/env bash
    set -euo pipefail
    args=()
    needs_vm_signature=0
    for package in "$@"; do
      case "$package" in
        pvisor) package=pvisor ;;
        core|control|agentctl) package=pvisor-core ;;
        capture) package=pvisor-gateway ;;
        shim) package=pvisor-shim ;;
      esac
      if [[ "$package" == pvisor-vm || "$package" == pvisor || "$package" == nativepvisor ]]; then needs_vm_signature=1; fi
      args+=(-p "$package")
    done
    if [[ $# -eq 0 ]]; then args+=(--workspace); needs_vm_signature=1; fi
    if [[ "$needs_vm_signature" == 1 && "$(uname -s)" == Darwin ]]; then
      python3 scripts/sign-vm-tests.py "${args[@]}"
    fi
    cargo nextest run --locked "${args[@]}"

# Cross-check the containerd shim for Linux; full builds need a Linux host.
shim-check:
    cargo check --locked -p pvisor-shim --target x86_64-unknown-linux-musl
    cargo clippy --locked -p pvisor-shim --all-targets --target x86_64-unknown-linux-musl -- -D warnings

# Build the static musl shim with the pvisor-vm executor (needs zigbuild).
shim-vm-build:
    python3 scripts/build-pvisor.py --shim-vm --profile dev --target-dir "{{ target_dir }}"

# Python tests; append pytest options such as -v or -k packaging.
test-py *args:
    uv run --extra dev pytest -q "$@"

# Strict Linux rootless/FUSE regression: never skip missing user namespaces.
test-isolation:
    env -u PVISOR_TEST_ALLOW_NO_USERNS cargo nextest run --locked -p pvisor --test rootless_local --test run_config_cli --no-capture

# Build the debug CLI and check its main command surfaces.
smoke: build
    #!/usr/bin/env bash
    set -euo pipefail
    for command in run status inspect apply drop tui replay; do
      "{{ target_dir }}/debug/pvisor" "$command" --help >/dev/null
    done
    "{{ target_dir }}/debug/pvisor" status --help | grep -Fq -- '--review'

# Run all examples, or pass scenario directory names to select a subset.
examples *scenarios: (build "release")
    PVISOR_BIN="{{ target_dir }}/release/pvisor" bash examples/pvisor/test.sh "$@"

# Run DOC specifications and save JSON; select S-DOC IDs with --case.
cases *args: (build "release") vm-case-driver
    cargo run --quiet --manifest-path tools/semspec/Cargo.toml --locked -- --config semspec-doc.toml run --domain DOC --subject-bin "{{ target_dir }}/release/pvisor" --format json --output "{{ target_dir }}/pvisor-case-report.json" "$@"

# Build/sign the native SDK driver and run VM control/backing DOC cases.
vm-case-driver:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --locked -p pvisor --release --example vm_control_case --target-dir "{{ target_dir }}"
    driver="{{ target_dir }}/release/examples/vm_control_case"
    test -x "$driver"
    if [[ "$(uname -s)" == Darwin ]]; then
      codesign --force --sign - --entitlements "{{ repo }}/crates/pvisor/macos-hypervisor.entitlements" "$driver"
      codesign --verify --strict "$driver"
    fi

vm-cases *args: (build "release") vm-case-driver
    PVISOR_CASE_VM_DRIVER="{{ target_dir }}/release/examples/vm_control_case" cargo run --quiet --manifest-path tools/semspec/Cargo.toml --locked -- --config semspec-doc.toml run docs/src/zh/reference/cases-vm.md --domain DOC --subject-bin "{{ target_dir }}/release/pvisor" --format json --output "{{ target_dir }}/pvisor-vm-case-report.json" "$@"

# Measure process startup and Run Bundle access (smoke or nightly).
benchmark suite="smoke" output="benchmark/pvisor/.data/process-current" build_dir="target/pvisor-benchmark-build":
    bash benchmark/pvisor/run.sh run --suite "$1" --output "$2" --target-dir "$3"

# Build, preflight, and benchmark all available sandbox cases in one command.
benchmark-startup *args:
    python3 benchmark/pvisor/run_all.py "$@"

# Run the low-level startup harness with explicit rootfs/image inputs.
benchmark-startup-raw *args:
    python3 benchmark/pvisor/startup.py --output benchmark/pvisor/.data/startup "$@"

# Compare reports from the same host; an empty baseline is allowed.
benchmark-compare candidate baseline="" output="benchmark/pvisor/.data/comparison" threshold="15":
    bash benchmark/pvisor/run.sh compare --candidate "$1" --baseline "$2" --output "$3" --regression-threshold "$4"

test-benchmark *args:
    just test-py benchmark/pvisor "$@"

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

# Build/test the independent semantic specification tool without product dependencies.
test-semspec *args:
    cargo nextest run --manifest-path tools/semspec/Cargo.toml --locked "$@"

# Semantic specification CLI; approvals are interactive, human-only actions.
semspec *args:
    cargo run --quiet --manifest-path tools/semspec/Cargo.toml --locked -- "$@"

# Verify the first review domain in fresh temporary workspaces. DOC remains in just cases.
semantics *args: (build "debug")
    cargo run --quiet --manifest-path tools/semspec/Cargo.toml --locked -- run --domain STAGE --subject-bin "{{ target_dir }}/debug/pvisor" "$@"

# New CLI learning path: every selected case must actually PASS (no SKIP/XFAIL).
cases-v2 *args: (build "release")
    python3 scripts/cases/run.py --subject-bin "{{ target_dir }}/release/pvisor" --output "{{ target_dir }}/pvisor-learning-report.json" "$@"

# Real macOS HVF CPU/RAM cold-restore validation (M0, not full guest recovery).
test-hvf-cold-restore:
    python3 scripts/check-hvf-cold-restore.py --target-dir "{{ target_dir }}"

# VMM owning-thread/GIC correctness checks.
test-vm-snapshot-state:
    python3 scripts/check-vm-snapshot-state.py --target-dir "{{ target_dir }}"

