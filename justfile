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
    for name in pvisor pvisor-cache pvisor-tui pvisor-replay pvisor-memory-pool pvisor-cluster pvisor-worker; do
      binary="{{ target_dir }}/$1/$name"
      test -x "$binary"
      if [[ "$(uname -s)" == Darwin ]]; then
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
    for binary in pvisor pvisor-cache pvisor-tui pvisor-replay pvisor-memory-pool pvisor-cluster pvisor-worker; do
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

# Build the distributed controller and native per-host worker.
cluster-build:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --locked -p pvisor-cluster -p pvisor --bin pvisor-cluster --bin pvisor-worker
    if [[ "$(uname -s)" == Darwin ]]; then
      codesign --force --sign - --entitlements "{{ repo }}/crates/pvisor/macos-hypervisor.entitlements" "{{ target_dir }}/debug/pvisor-worker"
      codesign --verify --strict "{{ target_dir }}/debug/pvisor-worker"
    fi

# Unified service component set, with optional Worker Gateway support.
service-build:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --locked -p pvisor -p pvisor-cluster --bin pvisor --bin pvisor-cache --bin pvisor-worker --bin pvisor-cluster --bin pvisor-memory-pool --features pvisor/gateway
    if [[ "$(uname -s)" == Darwin ]]; then
      for name in pvisor pvisor-worker pvisor-memory-pool; do
        codesign --force --sign - --entitlements "{{ repo }}/crates/pvisor/macos-hypervisor.entitlements" "{{ target_dir }}/debug/$name"
      done
    fi

# Real service lifecycle and same-host read-only backing ownership; no guest VMs.
test-service: service-build
    cargo nextest run --locked -p pvisor --features gateway --test service_execution --run-ignored all -E 'not test(native_vm_workers)' --test-threads 1

# Two real KVM/FUSE VMs, each 128 MiB/one vCPU in a separately capped Worker.
test-service-vm: service-build
    cargo nextest run --locked -p pvisor --features gateway --test service_execution --run-ignored only -E 'test(native_vm_workers)' --test-threads 1

# Controller and Worker with Attempt-local model Gateway support.
cluster-build-gateway:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --locked -p pvisor-cluster -p pvisor --features pvisor/gateway --bin pvisor-cluster --bin pvisor-worker
    if [[ "$(uname -s)" == Darwin ]]; then
      codesign --force --sign - --entitlements "{{ repo }}/crates/pvisor/macos-hypervisor.entitlements" "{{ target_dir }}/debug/pvisor-worker"
      codesign --verify --strict "{{ target_dir }}/debug/pvisor-worker"
    fi

# Controller contracts plus real HTTP/multi-worker execution and failure tests.
test-cluster:
    just test pvisor-cluster pvisor-core
    cargo nextest run --locked -p pvisor --test cluster_execution --bin pvisor-worker

# Explicit feature build: Worker Gateway, common contracts and native model protocols.
test-cluster-gateway:
    cargo nextest run --locked -p pvisor -p pvisor-core -p pvisor-cluster -p pvisor-gateway -p pvisor-journal --features pvisor/gateway

# Each hardware gate owns host timing/resource measurements. Concurrency within
# a gate remains real; unrelated gates run sequentially to avoid interference.
# Actual Linux KVM/FUSE environments and remote VM controls; missing devices fail.
test-cluster-vm:
    cargo nextest run --locked -p pvisor --test cluster_environment_vm --run-ignored only --test-threads 1

# Actual model/tool loops in immutable-environment VMs with Attempt-local Gateways.
test-cluster-vm-gateway:
    cargo build --locked -p pvisor-cluster --bin pvisor-cluster
    PVISOR_TEST_CONTROLLER_BINARY="{{ target_dir }}/debug/pvisor-cluster" cargo nextest run --locked -p pvisor --features gateway --test cluster_gateway_vm --run-ignored only --test-threads 1

# Dedicated user-systemd cgroup with real VM restore, CPU overcommit and cleanup.
test-cluster-cgroup:
    PVISOR_TEST_WORKER_SYSTEMD=1 cargo nextest run --locked -p pvisor --test cluster_environment_vm --run-ignored only -E 'test(concurrent_restores_share_physical_ram_baseline_and_keep_private_writes)'

# Explicit paired SMT experiment; independent of correctness/hardware gates.
bench-cluster-cpu:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${PVISOR_CPU_BENCH_OUT:?set an absolute JSON output path}"
    cargo nextest run --locked -p pvisor --test cluster_cpu_benchmark --run-ignored only

# Finite CPU/RAM envelope with result-checked native Agent/model/tool work.
bench-cluster-inference:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${PVISOR_INFERENCE_BENCH_OUT:?set an absolute JSON output path}"
    cargo nextest run --locked -p pvisor --features gateway --test cluster_inference_benchmark --run-ignored only --test-threads 1

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
      if [[ "$package" == pvisor-vm ]]; then needs_vm_signature=1; fi
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
benchmark suite="smoke" output="target/pvisor-benchmark/current" build_dir="target/pvisor-benchmark-build":
    bash benchmark/pvisor/run.sh run --suite "$1" --output "$2" --target-dir "$3"

# Build, preflight, and benchmark all available sandbox cases in one command.
benchmark-startup *args:
    python3 benchmark/pvisor/run_all.py "$@"

# Run the low-level startup harness with explicit rootfs/image inputs.
benchmark-startup-raw *args:
    python3 benchmark/pvisor/startup.py --output target/pvisor-benchmark/startup "$@"

# Compare reports from the same host; an empty baseline is allowed.
benchmark-compare candidate baseline="" output="target/pvisor-benchmark/comparison" threshold="15":
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

# VMM owning-thread/GIC checks; full Linux snapshot acceptance is separate.
test-vm-snapshot-state:
    python3 scripts/check-vm-snapshot-state.py --target-dir "{{ target_dir }}"

# Historical standalone snapshot gate: the current CLI no longer exposes it.
# Use archived binaries with benchmark/pvisor/vm_stress.py to reproduce old evidence.
test-vm-stress output cycles="5" forks="4" seed="1":
    @echo 'Standalone snapshot CLI is removed. Use an archived binary for vm_stress.py; current capped environment-sharing gate: just test-service-vm.' >&2
    @exit 2
