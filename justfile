# Persisting — 仓库主任务入口
# 安装：brew install just / cargo install just
#
repo := justfile_directory()
docs_dir := repo / "docs"

# Product CLI component set. Keep Cargo build entry points below routed
# through `build-components` so package/bin changes have one source in just.
component_pvisor := "-p persisting-pvisor --bin pvisor"

# Python 路径（ruff format）
ruff_paths := "persisting tests examples"
# lint 默认只扫包代码（与 CI 一致）；全量用 lint-py-all
ruff_lint_paths := "persisting"

# ── 帮助 ─────────────────────────────────────────────────────────────────────

default:
    @just --list --unsorted
    @echo ""
    @echo "常用："
    @echo "  just dev                 # 提交前（fmt + lint + test-rust）"
    @echo "  just test [package]      # 日常功能测试（可指定 Cargo 包）"
    @echo "  just install-cli         # 安装 pvisor"
    @echo "  just pvisor              # 构建 release pVisor；macOS 自动签名"
    @echo "  just examples-pvisor     # 构建并验证全部 pVisor examples"
    @echo "  just benchmark-pvisor    # pVisor 进程启动与 Bundle 访问基准"
    @echo "  just build-wheel         # 打 release wheel → dist/"
    @echo "  just docs-serve          # 本地文档"

# ── 测试套件导航 ──────────────────────────────────────────────────────────────

# 列出推荐测试。
[group('test')]
test-list:
    #!/usr/bin/env bash
    set -euo pipefail
    cat <<'EOF'
    Persisting 测试入口

      门禁 / Rust（just）
        just dev                  提交前：fmt + lint + test-rust
        just ci                   CI 近似
        just test [package]        日常功能测试；可指定 Cargo 包
        just capture-test / test-py  其他定向测试入口
        （Rust 测试由 cargo nextest 执行；文档测试仍用 cargo test）
        just cases pvisor
        just cases pvisor --run-unavailable --keep

      组件示例
        just examples-pvisor              全部 pVisor 场景
        just examples-pvisor-filesystem   需要 FUSE 的 01/02 场景
        just examples-pvisor-portable     普通 runner 可跑的 03/04 场景
        just example-pvisor 03-network-isolation

      pVisor 回归 / 基准
        just test-pvisor / test-pvisor-isolation
        just smoke-pvisor-cli
        just benchmark-pvisor             快速 smoke 基准
        just benchmark-pvisor nightly     稳定分布基准
    EOF

# Run and validate every deterministic, quantitative pVisor example.
[group('test')]
examples-pvisor profile="release": (pvisor profile)
    bash examples/pvisor/test.sh --profile "{{ profile }}" \
      01-filesystem-isolation \
      02-changeset-management \
      03-network-isolation \
      04-gateway-llm-control

# Run and validate the FUSE-backed workspace and changeset examples.
[group('test')]
examples-pvisor-filesystem profile="release": (pvisor profile)
    bash examples/pvisor/test.sh --profile "{{ profile }}" \
      01-filesystem-isolation 02-changeset-management

# Run and validate pVisor examples that do not require FUSE or user namespaces.
[group('test')]
examples-pvisor-portable profile="release": (pvisor profile)
    bash examples/pvisor/test.sh --profile "{{ profile }}" \
      03-network-isolation 04-gateway-llm-control

# Run and validate one named pVisor example.
[group('test')]
example-pvisor scenario profile="release": (pvisor profile)
    bash examples/pvisor/test.sh --profile "{{ profile }}" "{{ scenario }}"

examples: examples-pvisor

# Run pVisor's process-level startup and durable Run Bundle benchmark. The
# smoke suite is intended for PR CI; nightly raises warmups and sample counts.
[group('benchmark')]
benchmark-pvisor suite="smoke" output="target/pvisor-benchmark/current" target_dir="target/pvisor-benchmark-build":
    bash benchmark/pvisor/run.sh run \
      --suite "{{ suite }}" \
      --output "{{ output }}" \
      --target-dir "{{ target_dir }}"

# Compare reports from the same host. A missing baseline is valid for the first
# commit that introduces the benchmark and produces a candidate-only report.
[group('benchmark')]
benchmark-pvisor-compare candidate baseline="" output="target/pvisor-benchmark/comparison" regression_threshold="15":
    bash benchmark/pvisor/run.sh compare \
      --candidate "{{ candidate }}" \
      --baseline "{{ baseline }}" \
      --output "{{ output }}" \
      --regression-threshold "{{ regression_threshold }}"

# Unit-test the benchmark report and comparison contract without running it.
[group('test')]
test-pvisor-benchmark:
    PYTHONDONTWRITEBYTECODE=1 python3 benchmark/pvisor/test_bench.py

# ── 构建 ─────────────────────────────────────────────────────────────────────

# Thin forward to `build-components` for the product CLI.
[group('build')]
build profile="debug":
    just build-components "{{ profile }}" all

# Single Cargo build entry for the product CLI. Building `pvisor` also applies
# the macOS Hypervisor entitlement when running on Darwin.
[group('build')]
build-components profile="debug" components="all":
    #!/usr/bin/env bash
    set -euo pipefail
    profile="{{ profile }}"
    components="{{ components }}"
    case "$profile" in
      debug) cargo_profile=dev ;;
      release) cargo_profile=release ;;
      *) echo "unsupported build profile: $profile (expected debug or release)" >&2; exit 2 ;;
    esac

    case "$components" in
      all|runtime|pvisor)
        cargo build --profile "$cargo_profile" --locked {{ component_pvisor }}
        ;;
      *)
        echo "unsupported component set: $components (all|pvisor)" >&2
        exit 2
        ;;
    esac

    just _sign-pvisor "$profile"

# macOS HVF entitlement for the pVisor binary produced by `build-components`.
[private]
_sign-pvisor profile:
    #!/usr/bin/env bash
    set -euo pipefail
    profile="{{ profile }}"
    case "$profile" in
      debug|release) ;;
      *) echo "unsupported pVisor profile: $profile (expected debug or release)" >&2; exit 2 ;;
    esac
    binary="{{ repo }}/target/$profile/pvisor"
    test -x "$binary"
    if [[ "$(uname -s)" != "Darwin" ]]; then
      echo "Built pVisor: $binary"
      exit 0
    fi
    entitlements="{{ repo }}/crates/persisting-pvisor/macos-hypervisor.entitlements"
    command -v codesign >/dev/null
    codesign --force --sign - --entitlements "$entitlements" "$binary"
    codesign --verify --strict --verbose=2 "$binary"
    codesign -d --entitlements :- "$binary" 2>&1 \
      | grep -q 'com.apple.security.hypervisor'
    echo "Built and signed pVisor: $binary"

# Build (and on macOS, sign) pVisor. Thin forward to `build-components`.
# Usage: `just pvisor` (release) or `just pvisor debug`.
[group('build')]
pvisor profile="release":
    just build-components "{{ profile }}" pvisor

# Install the pVisor CLI.
install-cli:
    #!/usr/bin/env bash
    set -euo pipefail
    install_root="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"
    cargo install --path crates/persisting-pvisor --locked --force --root "$install_root"
    printf 'Installed pVisor in %s/bin\n' "$install_root"

# PEP 517 release wheel（Python package + pvisor）→ dist/
build-wheel:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p dist
    uv build --force-pep517 --wheel --out-dir dist
    wheel=$(ls -t dist/*.whl | head -n 1)
    python3 scripts/packaging/verify_wheel.py "$wheel" --install-smoke
    ls -la "$wheel"

# 开发调试 wheel（dev profile，不 strip）
build-wheel-debug:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p dist
    uv build --force-pep517 --wheel --out-dir dist \
      --config-setting 'cargo-profile=dev'
    wheel=$(ls -t dist/*.whl | head -n 1)
    python3 scripts/packaging/verify_wheel.py "$wheel" --install-smoke
    ls -la "$wheel"

clean:
    cargo clean
    rm -rf dist target/wheels .venv htmlcov .coverage coverage.xml

# ── 格式化 / Lint ─────────────────────────────────────────────────────────────

# 格式化 Rust + Python（会改写文件）
fmt: fmt-rust fmt-py

fmt-rust:
    cargo fmt --all

fmt-py:
    uvx ruff format {{ ruff_paths }}

# 只检查格式，不改写（CI / pre-commit）
fmt-check: fmt-check-rust fmt-check-py

fmt-check-rust:
    cargo fmt --all -- --check

fmt-check-py:
    uvx ruff format --check {{ ruff_paths }}

# clippy + ruff（不改写）
lint: lint-rust lint-py

lint-rust: clippy-deny

lint-py:
    uvx ruff check {{ ruff_lint_paths }}

# 含 tests/examples（较严，可能有存量告警）
lint-py-all:
    uvx ruff check {{ ruff_paths }}

clippy-deny:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# 兼容旧名
clippy:
    just lint-rust

# 自动修：format + ruff --fix
fix: fmt
    uvx ruff check {{ ruff_paths }} --fix

# 仅修 Python
fix-py: fmt-py
    uvx ruff check {{ ruff_paths }} --fix

# 格式 + lint 快检（不跑测试）
style: fmt-check lint
    @echo "✅ format + lint OK"

# fmt + lint + Rust 测试（日常 / 提交前）
[group('test')]
dev:
    just fmt
    just lint
    just test-rust

# 与 GitHub Actions `ci.yml` lint 对齐（只检查、不改写）
ci-lint:
    just fmt-check-rust
    just lint-rust
    just lint-py

# CI 近似：功能门禁 + 构建
ci:
    just dev
    just build

# ── Rust 测试 ─────────────────────────────────────────────────────────────────

# CI shard helper: `just ci-nextest persisting-gateway persisting-events …`
# Variadic args are interpolated by just (not passed as shebang $@).
[group('test')]
ci-nextest +packages:
    #!/usr/bin/env bash
    set -euo pipefail
    args=()
    for pkg in {{ packages }}; do
      args+=(-p "$pkg")
    done
    cargo nextest run --locked "${args[@]}"

# 单 crate：agentctl | capture | pvisor
test-crate crate:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{ crate }}" in
      agentctl) cargo nextest run -p persisting-agentctl --locked ;;
      capture) cargo nextest run -p persisting-gateway --locked ;;
      pvisor) cargo nextest run -p persisting-pvisor --locked ;;
      *) echo "unknown crate: {{ crate }} (agentctl|capture|pvisor)" >&2; exit 2 ;;
    esac

test-rust package="":
    #!/usr/bin/env bash
    set -euo pipefail
    package="{{ package }}"
    if [[ -n "$package" ]]; then
        cargo nextest run --locked -p "$package"
    else
        cargo nextest run --workspace --locked
    fi

# Default pVisor crate profile, including CLI and integration regressions.
[group('test')]
test-pvisor:
    cargo nextest run -p persisting-pvisor --locked

# Strict Linux rootless/FUSE boundary tests. This deliberately does not allow
# the optional-userns skip used by the broad cross-platform workspace job.
[group('test')]
test-pvisor-isolation:
    env -u PERSISTING_TEST_ALLOW_NO_USERNS \
      cargo nextest run -p persisting-pvisor --test rootless_local --locked -- --nocapture

# Product CLI surface exercised by CI after the debug component build.
[group('test')]
smoke-pvisor-cli:
    target/debug/pvisor run --help >/dev/null
    target/debug/pvisor status --help >/dev/null
    target/debug/pvisor review --help >/dev/null

test-capture-claude:
    cargo nextest run -p persisting-gateway --test capture_apps_claude --locked

test-capture-fixtures:
    cargo nextest run -p persisting-gateway --locked --test llm_fixtures --test ag_fixture_tests

test-capture-network:
    cargo nextest run -p persisting-gateway --locked --lib network_policy
    cargo nextest run -p persisting-gateway --locked --test network_policy_http

# Rust + Python. Rust tests run debug-mode nextest for faster iteration; use
# `just test-rust` with a package for targeted coverage. Passing a package runs
# only that Rust package. The full Python suite runs only for the no-argument
# repository-wide invocation.
test package="":
    #!/usr/bin/env bash
    set -euo pipefail
    package="{{ package }}"
    if [[ -z "$package" ]]; then
      just test-rust
      just test-py
      exit 0
    fi
    case "$package" in
      agentctl|capture|pvisor)
        just test-crate "$package"
        ;;
      *)
        just test-rust "$package"
        ;;
    esac

# ── Python ───────────────────────────────────────────────────────────────────

# 同步纯 Python 开发环境
py-dev:
    uv sync --all-extras

test-py:
    uv run pytest tests/ -q

test-py-v:
    uv run pytest tests/ -v

# 安装本地 nightly 脚本自检（需已有 GitHub nightly release）
install-nightly:
    bash "{{ repo }}/scripts/install-nightly.sh"

# ── 文档（docs/ 子项目）──────────────────────────────────────────────────────

docs-sync:
    cd "{{ docs_dir }}" && if [[ ! -x .venv/bin/zensical ]]; then uv venv .venv && UV_CACHE_DIR=/tmp/uv-cache uv pip install --python .venv/bin/python zensical==0.0.61; fi

docs-serve: docs-sync
    cd "{{ docs_dir }}" && .venv/bin/python "{{ repo }}/scripts/build-docs.py" && .venv/bin/python "{{ repo }}/scripts/serve-docs.py" --host 127.0.0.1 --port 3000 --directory site

docs-serve-dirty: docs-sync
    cd "{{ docs_dir }}" && .venv/bin/python "{{ repo }}/scripts/build-docs.py" && .venv/bin/python "{{ repo }}/scripts/serve-docs.py" --host 127.0.0.1 --port 3000 --directory site --watch

docs-build: docs-sync
    cd "{{ docs_dir }}" && .venv/bin/python "{{ repo }}/scripts/build-docs.py"

check-quick:
    cargo check \
      -p persisting-agentctl \
      -p persisting-events \
      -p persisting-gateway \
      -p persisting-pvisor \
      --locked

# capture 相关 Rust 测试（Gateway 包测试已覆盖全部 capture targets）。
capture-test:
    just test-crate capture

# Run documented pVisor integration cases.
# Extra runner flags can be passed directly, e.g.
#   just cases pvisor --run-unavailable --keep
#   just cases pvisor --case A01,A02 --case B01
[group('test')]
cases target *args:
    #!/usr/bin/env bash
    set -euo pipefail
    # Variadic args are interpolated by just (not shebang $@). A leading `--`
    # may be present when callers stop just flag parsing; strip it.
    set -- {{ args }}
    if [[ "${1:-}" == "--" ]]; then
      shift
    fi
    case "{{target}}" in
      pvisor)
        just pvisor release
        python3 scripts/run-pvisor-cases.py --report target/pvisor-case-report.md "$@"
        ;;
      *)
        echo "usage: just cases pvisor [runner-args...]" >&2
        exit 2
        ;;
    esac
