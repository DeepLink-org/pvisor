# PolicyVisor development tasks. Run `just` to list public recipes by group.
set positional-arguments

repo := justfile_directory()
target_dir := absolute_path(env("CARGO_TARGET_DIR", repo / "target"))
python_paths := "pvisor tests examples benchmark/pvisor/test_*.py"
zensical_version := "0.0.67"

import "tests/justfile"

# ============================================================
# 01. 通用入口
# ============================================================

# 按分组展示公开配方
[group("01. 通用入口")]
default:
    @just --list --unsorted

# 顺序执行 Just 命令；无参数时执行默认本地检查序列（带参数的命令用引号包住）
[group("01. 通用入口")]
ci *commands:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ $# -eq 0 ]]; then
      set -- "fmt-rust --check" "fmt-py --check" lint test build
    fi
    for command in "$@"; do
      read -r -a words <<< "$command"
      if [[ ${#words[@]} -eq 0 ]]; then echo "empty CI command" >&2; exit 2; fi
      just -- "${words[@]}"
    done

# 清理构建产物，保留开发环境和运行记录
[group("01. 通用入口")]
clean:
    cargo clean --target-dir "{{ target_dir }}" --profile dev
    cargo clean --target-dir "{{ target_dir }}" --profile release
    cargo clean --target-dir "{{ target_dir }}" --profile performance
    cargo clean --manifest-path tools/semspec/Cargo.toml --profile dev
    rm -rf build dist docs/site "{{ target_dir }}/fw"

# ============================================================
# 02. 构建与打包
# ============================================================

# 支持 debug / release / performance
[group("02. 构建与打包")]
build profile="debug":
    #!/usr/bin/env bash
    set -euo pipefail
    case "$1" in
      debug) cargo_profile=dev ;;
      release) cargo_profile=release ;;
      performance) cargo_profile=performance ;;
      *) echo "expected debug, release or performance, got: $1" >&2; exit 2 ;;
    esac
    python3 scripts/packaging/stage_wheel_binaries.py --profile "$cargo_profile" --target-dir "{{ target_dir }}"
    names=(pvisor pvisor-cache pvisor-tui pvisor-replay)
    if [[ "$(uname -s)-$(uname -m)" == Linux-x86_64 ]]; then names+=(pvisor-daemon); fi
    for name in "${names[@]}"; do
      binary="{{ target_dir }}/$1/$name"
      test -x "$binary"
      if [[ "$(uname -s)" == Darwin ]]; then
        codesign --force --sign - --entitlements "{{ repo }}/crates/pvisor/macos-hypervisor.entitlements" "$binary"
        codesign --verify --strict "$binary"
      fi
    done

# 产品编译检查
[group("02. 构建与打包")]
check:
    cargo check --locked -p pvisor-cli

# 支持 release / debug；验证通过后输出 wheel
[group("02. 构建与打包")]
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
    if [[ "${PVISOR_WHEEL_PORTABLE:-0}" == 1 ]]; then
      export CIBW_CONFIG_SETTINGS="cargo-profile=$cargo_profile"
      uvx --from cibuildwheel==4.1.0 cibuildwheel --output-dir "$staging"
    else
      uv build --wheel --out-dir "$staging" --config-setting "cargo-profile=$cargo_profile"
    fi
    for wheel in "$staging"/pvisor-*.whl; do
      python3 scripts/packaging/verify_wheel.py "$wheel" --install-smoke
    done
    uvx twine check "$staging"/pvisor-*.whl
    mkdir -p dist
    mv "$staging"/pvisor-*.whl dist/
    for source in "$staging"/pvisor-firmware-source-*.tar.gz; do
      if [[ -f "$source" ]]; then mv "$source" dist/; fi
    done

# ============================================================
# 03. 固件
# ============================================================

# action: build / test；其余参数传给对应操作
[group("03. 固件")]
fw action *args:
    #!/usr/bin/env bash
    set -euo pipefail
    action="$1"
    shift
    case "$action" in
      build) python3 scripts/packaging/firmware.py --target-dir "{{ target_dir }}" "$@" ;;
      test) uv run --no-project --with pyelftools==0.33 python -m unittest discover -s fw/tests -v "$@" ;;
      *) echo "expected build or test, got: $action" >&2; exit 2 ;;
    esac

# ============================================================
# 04. 格式化
# ============================================================

# 格式化受维护的 Rust 与 Python 源码
[group("04. 格式化")]
fmt: fmt-rust fmt-py

# 支持 --check 等 formatter 参数
[group("04. 格式化")]
fmt-rust *args:
    cargo fmt --all -- "$@"

# 支持 --check 等 formatter 参数
[group("04. 格式化")]
fmt-py *args:
    uvx ruff format {{ python_paths }} "$@"

# ============================================================
# 05. 静态检查
# ============================================================

# 聚合 Rust、Python 和 workflow lint
[group("05. 静态检查")]
lint: lint-rust lint-py lint-workflows

# Rust 静态检查
[group("05. 静态检查")]
lint-rust:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# Python 静态检查
[group("05. 静态检查")]
lint-py:
    uvx ruff check {{ python_paths }}

# 使用本机 actionlint；CI 和本地使用同一入口
[group("05. 静态检查")]
lint-workflows:
    actionlint

# ============================================================
# 06. 测试与冒烟
# ============================================================

# 无参数：Rust + Python；传包名：仅指定 Rust 包
[group("06. 测试与冒烟")]
test *packages:
    #!/usr/bin/env bash
    set -euo pipefail
    just test-rust "$@"
    if [[ $# -eq 0 ]]; then just test-scripts; just test-py; fi

# Debug nextest；支持 Cargo 包名和已有简称，-- 后转发 nextest 参数
[group("06. 测试与冒烟")]
test-rust *packages:
    #!/usr/bin/env bash
    set -euo pipefail
    args=()
    needs_vm_signature=0
    while [[ $# -gt 0 && "$1" != -- ]]; do
      package="$1"
      shift
      case "$package" in
        pvisor) package=pvisor ;;
                cli) package=pvisor-cli ;;
        core|control|agentctl) package=pvisor-core ;;
        capture) package=pvisor-gateway ;;
        shim) package=pvisor-shim ;;
      esac
      if [[ "$package" == pvisor-vm || "$package" == pvisor || "$package" == pvisor-cli ]]; then needs_vm_signature=1; fi
      args+=(-p "$package")
    done
    if [[ ${#args[@]} -eq 0 ]]; then args+=(--workspace); needs_vm_signature=1; fi
    if [[ "$needs_vm_signature" == 1 && "$(uname -s)" == Darwin ]]; then
      just sign-vm-tests "${args[@]}"
    fi
    if [[ "${1:-}" == -- ]]; then shift; fi
    cargo nextest run --locked "${args[@]}" "$@"

# 标准库 unittest；支持文件、测试名、-k/-v，或 discover 参数
# 无参数时覆盖 tests/ 和 benchmark/pvisor/，目录选择用 discover -s DIR
[group("06. 测试与冒烟")]
test-py *args:
    #!/usr/bin/env bash
    set -euo pipefail
    export PYTHONPATH="{{ repo }}/benchmark/pvisor${PYTHONPATH:+:$PYTHONPATH}"
    selected=0
    option_value=0
    for arg in "$@"; do
      if [[ "$option_value" == 1 ]]; then option_value=0; continue; fi
      case "$arg" in
        -k|-s|-p|-t|--start-directory|--pattern|--top-level-directory) option_value=1 ;;
        -*) ;;
        *) selected=1 ;;
      esac
    done
    if [[ "$selected" == 0 ]]; then
      uv run --no-project --with 'setuptools>=77' python -m unittest discover -s tests "$@"
      uv run --no-project --with 'setuptools>=77' python -m unittest discover -s benchmark/pvisor "$@"
    else
      uv run --no-project --with 'setuptools>=77' python -m unittest "$@"
    fi

# 严格隔离回归，不因缺少系统能力而静默跳过
[group("06. 测试与冒烟")]
test-isolation:
    env -u PVISOR_TEST_ALLOW_NO_USERNS cargo nextest run --locked -p pvisor-cli --test rootless_local --test run_config_cli --no-capture

# 构建并检查主要 CLI 命令入口
[group("06. 测试与冒烟")]
smoke: build
    #!/usr/bin/env bash
    set -euo pipefail
    for command in run status inspect apply drop tui replay; do
      "{{ target_dir }}/debug/pvisor" "$command" --help >/dev/null
    done
    "{{ target_dir }}/debug/pvisor" status --help | grep -Fq -- '--review'

# ============================================================
# 07. 示例与语义场景
# ============================================================

# 不传参数运行默认场景集合；传参数选择场景
[group("07. 示例与语义场景")]
examples *scenarios: (build "release")
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ $# -eq 0 ]]; then
      set -- 01-filesystem-isolation 02-changeset-management 03-network-isolation 04-gateway-llm-control
    fi
    for scenario in "$@"; do
      case "$scenario" in
        01-filesystem-isolation|02-changeset-management|03-network-isolation|04-gateway-llm-control|05-zcode-cli|06-tui-interception) ;;
        *) echo "unknown pVisor example: $scenario" >&2; exit 2 ;;
      esac
    done
    export PVISOR_BIN="{{ target_dir }}/release/pvisor"
    export WORK_ROOT="${WORK_ROOT:-{{ target_dir }}/pvisor-examples}"
    for scenario in "$@"; do
      just "example-$scenario"
    done

# 统一场景验证入口；默认 DOC，可先传 --suite doc/stage/use/vm，保留选择与报告参数
[group("07. 示例与语义场景")]
cases *args:
    #!/usr/bin/env bash
    set -euo pipefail
    suite=doc
    if [[ "${1:-}" == --suite ]]; then
      if [[ $# -lt 2 ]]; then echo "--suite requires doc, stage, use or vm" >&2; exit 2; fi
      suite="$2"
      shift 2
    fi
    case "$suite" in
      doc|vm)
        just build release
        just vm-case-driver
        export PVISOR_CASE_VM_DRIVER="{{ target_dir }}/release/examples/vm_control_case"
        spec=docs/src/zh/reference
        report="{{ target_dir }}/pvisor-case-report.json"
        if [[ "$suite" == vm ]]; then
          spec=docs/src/zh/reference/cases-vm.md
          report="{{ target_dir }}/pvisor-vm-case-report.json"
        fi
        just semspec run "$spec" --domain DOC --subject-bin "{{ target_dir }}/release/pvisor" --format json --output "$report" "$@"
        ;;
      stage)
        just build debug
        just semspec run docs/src/zh/cases/06-stage-apply.md --domain STAGE --subject-bin "{{ target_dir }}/debug/pvisor" "$@"
        ;;
      use)
        just build release
        report="{{ target_dir }}/pvisor-learning-report.json"
        subject="{{ target_dir }}/release/pvisor"
        run_args=()
        while [[ $# -gt 0 ]]; do
          case "$1" in
            --output|--subject-bin|--case)
              if [[ $# -lt 2 ]]; then echo "$1 requires a value" >&2; exit 2; fi
              case "$1" in
                --output) report="$2" ;;
                --subject-bin) subject="$2" ;;
                --case) run_args+=(--case "$2") ;;
              esac
              shift 2
              ;;
            --output=*) report="${1#*=}"; shift ;;
            --subject-bin=*) subject="${1#*=}"; shift ;;
            --case=*) run_args+=(--case "${1#*=}"); shift ;;
            --keep|--require-reviewed) run_args+=("$1"); shift ;;
            *) echo "unknown USE case argument: $1" >&2; exit 2 ;;
          esac
        done
        just semspec run docs/src/zh/cases --domain USE --require-pass --subject-bin "$subject" --format json --output "$report" ${run_args[@]+"${run_args[@]}"}
        ;;
      *) echo "expected doc, stage, use or vm, got: $suite" >&2; exit 2 ;;
    esac

# ============================================================
# 08. VM 专项验证
# ============================================================

# 真实 macOS HVF CPU/RAM 冷恢复验证
[group("08. VM 专项验证")]
test-hvf-cold-restore: (vm-probe-build "pvisor-vm" "hvf_cold_restore_case")
    python3 scripts/check-hvf-cold-restore.py --binary "{{ target_dir }}/debug/examples/hvf_cold_restore_case"

# VMM 所有者线程与 GIC 状态检查
[group("08. VM 专项验证")]
test-vm-snapshot-state: (vm-probe-build "pvisor-vm" "threaded_cold_restore_case" "--offline")
    python3 scripts/check-vm-snapshot-state.py --binary "{{ target_dir }}/debug/examples/threaded_cold_restore_case"

# ============================================================
# 09. benchmark
# ============================================================

# 不传参数运行 smoke；传参数选择一个或多个已注册套件（B-PROCESS: smoke/nightly）
[group("09. benchmark")]
benchmark *suites:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ $# -eq 0 ]]; then set -- smoke; fi
    for suite in "$@"; do
      case "$suite" in
        smoke|nightly) ;;
        *) echo "unknown benchmark suite: $suite (expected smoke or nightly)" >&2; exit 2 ;;
      esac
    done
    benchmark_repo="${PVISOR_BENCHMARK_REPO:-{{ repo }}}"
    for suite in "$@"; do
      python3 "$benchmark_repo/benchmark/pvisor/bench.py" run --suite "$suite" --repo "$benchmark_repo" --output "${PVISOR_BENCHMARK_OUTPUT:-benchmark/pvisor/.data/process-current}/$suite" --target-dir "${PVISOR_BENCHMARK_BUILD_DIR:-{{ target_dir }}/pvisor-benchmark-build}"
    done

# ============================================================
# 10. 文档
# ============================================================

# 构建并检查双语文档；支持 --require-recorded
[group("10. 文档")]
docs-build *check_args:
    python3 scripts/check-reference.py
    rm -rf docs/site
    uv run --no-project --with zensical=={{ zensical_version }} zensical build --strict -f docs/zensical.zh.toml
    uv run --no-project --with zensical=={{ zensical_version }} zensical build --strict -f docs/zensical.en.toml
    cp docs/index.html docs/site/index.html
    python3 scripts/check-docs.py "$@"

# 本地 zh/en 预览；其余参数传给 Zensical
[group("10. 文档")]
docs-serve locale="zh" *args:
    #!/usr/bin/env bash
    set -euo pipefail
    case "$1" in
      zh|en) config="docs/zensical.$1.toml" ;;
      *) echo "expected zh or en, got: $1" >&2; exit 2 ;;
    esac
    shift
    exec uv run --no-project --with zensical=={{ zensical_version }} zensical serve -f "$config" "$@"

# ============================================================
# 11. 架构预算与发行校验
# ============================================================

# 检查运行时与应用依赖预算；其余参数传给校验器
[group("11. 架构预算与发行校验")]
check-core-budget *args:
    python3 scripts/ci/check_core_budget.py "$@"

# 检查版本、tag、主干关系与 lockfile；其余参数传给校验器
[group("11. 架构预算与发行校验")]
check-release-version *args:
    python3 scripts/ci/check_release_version.py "$@"

# 检查双平台发行制品的完整性与元数据；其余参数传给校验器
[group("11. 架构预算与发行校验")]
check-release-artifacts *args:
    python3 scripts/ci/check_release_artifacts.py "$@"

# ============================================================
# 12. 环境诊断
# ============================================================

# 只读诊断，不安装工具、不修改系统配置；suite: dev/build/test/fw/docs/isolation
[group("12. 环境诊断")]
doctor suite="dev":
    #!/usr/bin/env bash
    set -euo pipefail
    case "$1" in
      dev) tools=(just cargo rustc python3 uv jq actionlint); nextest=1 ;;
      build) tools=(just cargo rustc python3 uv); nextest=0 ;;
      test) tools=(just cargo python3 uv jq); nextest=1 ;;
      fw) tools=(python3 uv make cc); nextest=0 ;;
      docs) tools=(python3 uv); nextest=0 ;;
      isolation) tools=(cargo unshare fusermount3); nextest=1 ;;
      *) echo "unknown doctor suite: $1" >&2; exit 2 ;;
    esac
    missing=0
    for tool in "${tools[@]}"; do
      if command -v "$tool" >/dev/null; then
        printf '%s: %s\n' "$tool" "$(command -v "$tool")"
      else
        echo "$tool: missing" >&2
        missing=1
      fi
    done
    if [[ "$nextest" == 1 ]]; then cargo nextest --version || missing=1; fi
    if [[ "$1" == build || "$1" == dev ]]; then
      if [[ "$(uname -s)-$(uname -m)" == Linux-x86_64 ]]; then
        command -v zig >/dev/null || missing=1
        cargo zigbuild --version || missing=1
      fi
    fi
    if [[ "$1" == isolation ]]; then
      test -c /dev/fuse || missing=1
      unshare --user --map-root-user --mount --net /bin/true || missing=1
    fi
    exit "$missing"

# Internal/special-purpose recipes stay callable, but are absent from `just` help.

[private]
vm-probe-build package example *args:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$(uname -s)-$(uname -m)" != Darwin-arm64 ]]; then
      echo "VM restore probes require Apple Silicon macOS with Hypervisor entitlement" >&2
      exit 2
    fi
    package="$1"
    example="$2"
    shift 2
    cargo build --locked -p "$package" --example "$example" --target-dir "{{ target_dir }}" "$@"
    binary="{{ target_dir }}/debug/examples/$example"
    test -x "$binary"
    codesign --force --sign - --entitlements "{{ repo }}/crates/pvisor/macos-hypervisor.entitlements" "$binary"
    codesign --verify --strict "$binary"

[private]
vm-probe-guest:
    mkdir -p "{{ target_dir }}/debug/probes"
    rustc --edition 2024 --target aarch64-unknown-linux-musl -C linker=rust-lld -C opt-level=2 crates/pvisor-vm/src/probes/guest_linux.rs -o "{{ target_dir }}/debug/probes/guest_linux"

[private]
test-linux-cold-restore *args: (vm-probe-build "pvisor" "vm_linux_cold_restore" "--offline") vm-probe-guest
    python3 scripts/check-linux-cold-restore.py --binary "{{ target_dir }}/debug/examples/vm_linux_cold_restore" --guest-bin "{{ target_dir }}/debug/probes/guest_linux" --firmware-dir "${PVISOR_CASE_VM_LIBRARY_DIR:-{{ target_dir }}/release}" --report "{{ target_dir }}/vm-validation/linux-machine.json" "$@"

[private]
test-environment-snapshot *args: (vm-probe-build "pvisor" "vm_environment_snapshot" "--offline") vm-probe-guest
    python3 scripts/check-environment-snapshot.py --binary "{{ target_dir }}/debug/examples/vm_environment_snapshot" --guest-bin "{{ target_dir }}/debug/probes/guest_linux" --firmware-dir "${PVISOR_CASE_VM_LIBRARY_DIR:-{{ target_dir }}/release}" --report "{{ target_dir }}/vm-validation/environment-linux.json" "$@"

[private]
install-cli: (build "release")
    #!/usr/bin/env bash
    set -euo pipefail
    install_root="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"
    mkdir -p "$install_root/bin"
    binaries=(pvisor pvisor-cache pvisor-tui pvisor-replay)
    if [[ "$(uname -s)-$(uname -m)" == Linux-x86_64 ]]; then binaries+=(pvisor-daemon); fi
    for binary in "${binaries[@]}"; do
      install -m 755 "{{ target_dir }}/release/$binary" "$install_root/bin/$binary"
    done
    if [[ "$(uname -s)" == Darwin ]]; then
      install -m 755 "{{ target_dir }}/release/libkrunfw.5.dylib" "$install_root/bin/libkrunfw.5.dylib"
      install -m 644 "{{ target_dir }}/release/libkrunfw.SOURCE" "$install_root/bin/libkrunfw.SOURCE"
    fi

[private]
daemon-build profile="debug":
    #!/usr/bin/env bash
    set -euo pipefail
    case "$1" in
      debug) cargo_profile=dev ;;
      release|performance) cargo_profile="$1" ;;
      *) echo "expected debug, release or performance, got: $1" >&2; exit 2 ;;
    esac
    python3 scripts/packaging/stage_wheel_binaries.py --daemon --profile "$cargo_profile" --target-dir "{{ target_dir }}"

[private]
daemon-install: (daemon-build "release")
    #!/usr/bin/env bash
    set -euo pipefail
    install_root="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"
    mkdir -p "$install_root/bin"
    install -m 755 "{{ target_dir }}/release/pvisor-daemon" "$install_root/bin/pvisor-daemon"

[private]
shim-check:
    cargo check --locked -p pvisor-shim --target x86_64-unknown-linux-musl
    cargo clippy --locked -p pvisor-shim --all-targets --target x86_64-unknown-linux-musl -- -D warnings

[private]
shim-vm-build profile="debug":
    #!/usr/bin/env bash
    set -euo pipefail
    case "$1" in
      debug) cargo_profile=dev ;;
      release|performance) cargo_profile="$1" ;;
      *) echo "expected debug, release or performance, got: $1" >&2; exit 2 ;;
    esac
    python3 scripts/packaging/stage_wheel_binaries.py --shim-vm --profile "$cargo_profile" --target-dir "{{ target_dir }}"

[private]
sign-vm-tests *args:
    #!/usr/bin/env bash
    set -euo pipefail
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    cargo nextest list --locked --message-format json "$@" > "$work/suites.json"
    jq -j '."rust-suites"[] | select(."package-name" == "pvisor-vm" or ."package-name" == "pvisor" or ."package-name" == "pvisor-cli") | ."binary-path" + "\u0000"' "$work/suites.json" > "$work/binaries"
    while IFS= read -r -d '' binary; do
      codesign --force --sign - --entitlements "{{ repo }}/crates/pvisor/macos-hypervisor.entitlements" "$binary"
    done < "$work/binaries"

[private]
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

[private]
benchmark-startup *args:
    python3 benchmark/pvisor/run_all.py "$@"

[private]
benchmark-startup-raw *args:
    python3 benchmark/pvisor/startup.py --output benchmark/pvisor/.data/startup "$@"

[private]
test-semspec *args:
    cargo nextest run --manifest-path tools/semspec/Cargo.toml --locked "$@"

[private]
semspec *args:
    cargo run --quiet --manifest-path tools/semspec/Cargo.toml --locked -- "$@"

[private]
benchmark-compare candidate baseline="" output="benchmark/pvisor/.data/comparison" threshold="15":
    python3 benchmark/pvisor/bench.py compare --repo "{{ repo }}" --candidate "$1" --baseline "$2" --output "$3" --regression-threshold "$4"

[private]
set-nightly-version *args:
    python3 scripts/ci/set_nightly_local_version.py "$@"
    cargo metadata --format-version 1 >/dev/null

[private]
fmt-semspec:
    cargo fmt --manifest-path tools/semspec/Cargo.toml --check

[private]
lint-semspec:
    cargo clippy --manifest-path tools/semspec/Cargo.toml --all-targets --locked -- -D warnings

# cibuildwheel runs this inside its build environment, sharing the firmware receipt.
[private]
repair-wheel wheel destination:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$(uname -s)" == Darwin ]]; then
      cp "$1" "$2"
    else
      auditwheel repair -w "$2" "$1"
    fi
    python3 -m pip install pyelftools==0.33
    mkdir -p "$(dirname "$PVISOR_FW_SOURCE_ARCHIVE")"
    just fw build --source-output "{{ target_dir }}/libkrunfw.SOURCE" --source-archive "$PVISOR_FW_SOURCE_ARCHIVE"

[private]
example-01-filesystem-isolation:
    #!/usr/bin/env bash
    set -euo pipefail

    example_dir="{{ repo }}/examples/pvisor/01-filesystem-isolation"
    work_root="${WORK_ROOT:-$example_dir/.work}"
    work_dir="$work_root/filesystem-isolation"
    export WORK_ROOT="$work_root"

    bash "$example_dir/run.sh"

    base="$work_dir/base"
    run_dir="$(find "$work_dir/runs" -mindepth 1 -maxdepth 1 -type d -name 'run-*' -print -quit)"
    test -n "$run_dir"
    test "$(cat "$base/existing.txt")" = original
    test ! -e "$base/new.txt"
    test "$(cat "$run_dir/upper/existing.txt")" = changed
    test "$(cat "$run_dir/upper/new.txt")" = new
    jq -e '
      .run.state == "completed" and
      .filesystem.state == "staged" and
      .filesystem.changed_files == 2 and
      .safety.filesystem_changes_staged == true and
      .safety.filesystem_non_bypassable == true
    ' "$run_dir/run-bundle.json" >/dev/null

    echo 'RESULT example=filesystem-isolation base_unchanged=true staged_changes=2'

[private]
example-02-changeset-management:
    #!/usr/bin/env bash
    set -euo pipefail

    example_dir="{{ repo }}/examples/pvisor/02-changeset-management"
    work_root="${WORK_ROOT:-$example_dir/.work}"
    work_dir="$work_root/changeset-management"
    export WORK_ROOT="$work_root"

    bash "$example_dir/run.sh"

    base="$work_dir/base"
    jq -e '.run.state == "completed" and .filesystem.changed_files == 2' \
      "$work_dir/apply-review.json" >/dev/null
    jq -e '.run.state == "completed" and .filesystem.changed_files == 1' \
      "$work_dir/drop-review.json" >/dev/null
    test "$(cat "$base/existing.txt")" = accepted
    test "$(cat "$base/accepted.txt")" = accepted
    test ! -e "$base/rejected.txt"

    echo 'RESULT example=changeset-management reviewed=3 applied=2 dropped=1'

[private]
example-03-network-isolation:
    #!/usr/bin/env bash
    set -euo pipefail

    example_dir="{{ repo }}/examples/pvisor/03-network-isolation"
    work_root="${WORK_ROOT:-$example_dir/.work}"
    work_dir="$work_root/network-isolation"
    export WORK_ROOT="$work_root"

    command -v jq >/dev/null
    bash "$example_dir/run.sh"

    test "$(cat "$work_dir/allowed.status")" = 0
    test "$(tr -d '\r\n' <"$work_dir/allowed.stdout")" = allowed
    test "$(cat "$work_dir/denied.status")" != 0
    grep -q '(no-network)' "$work_dir/denied.stdout" "$work_dir/denied.stderr"
    test "$(cat "$work_dir/direct.status")" = 0
    test "$(tr -d '\r\n' <"$work_dir/direct.stdout")" = allowed

    bundle_count=0
    completed_count=0
    failed_count=0
    while IFS= read -r bundle; do
      bundle_count=$((bundle_count + 1))
      case "$(jq -r '.run.state' "$bundle")" in
        completed) completed_count=$((completed_count + 1)) ;;
        failed) failed_count=$((failed_count + 1)) ;;
        *) exit 1 ;;
      esac
    done < <(find "$work_dir/runs" -name run-bundle.json -type f -print)
    test "$bundle_count" = 3
    test "$completed_count" = 2
    test "$failed_count" = 1

    echo 'RESULT example=network-isolation allowed=true denied=true direct_bypass=true'

[private]
example-04-gateway-llm-control:
    #!/usr/bin/env bash
    set -euo pipefail

    example_dir="{{ repo }}/examples/pvisor/04-gateway-llm-control"
    work_root="${WORK_ROOT:-$example_dir/.work}"
    work_dir="$work_root/gateway-llm-control"
    export WORK_ROOT="$work_root"

    bash "$example_dir/run.sh"

    run_dir="$(find "$work_dir/runs" -mindepth 1 -maxdepth 1 -type d -name 'run-*' -print -quit)"
    test -n "$run_dir"
    upstream_posts="$(grep -c 'POST /v1/chat/completions' "$work_dir/mock.log")"
    test "$upstream_posts" = 2
    PYTHONPATH="$example_dir" python3 - "$run_dir/.capture/events.trace.jsonl" <<'PYTHON'
    import json
    import sys
    from pathlib import Path

    from dialogue_fixture import REPLIES, TURNS

    events = [json.loads(line) for line in Path(sys.argv[1]).read_text().splitlines()]
    assert events[0]["format"] == "pvisor.trace/5"
    llm = [
        {
            "kind": record["event"]["data"]["name"],
            "call_id": record["event"]["data"]["payload"]["correlation"]["call_id"],
            "payload": record["event"]["data"]["payload"]["content"],
        }
        for record in events[1:]
        if record["event"]["data"].get("name") in {"llm.request", "llm.response"}
    ]
    assert [event["kind"] for event in llm] == ["llm.request", "llm.response"] * 2
    assert len({event["call_id"] for event in llm}) == 2
    messages = []
    for index, (user, reply) in enumerate(zip(TURNS, REPLIES, strict=True)):
        request, response = llm[index * 2:index * 2 + 2]
        assert request["call_id"] == response["call_id"]
        messages.append({"role": "user", "content": user})
        assert request["payload"]["http"]["request_body"]["messages"] == messages
        assert response["payload"]["status"] == 200
        assert response["payload"]["assistant_content"] == reply
        messages.append({"role": "assistant", "content": reply})
    PYTHON
    jq -e '
      .run.state == "completed" and
      .network.intercepted.requests_seen == 2 and
      .network.intercepted.sink_requests == 2 and
      .network.intercepted.failures == 0
    ' "$run_dir/run-bundle.json" >/dev/null

    echo 'RESULT example=gateway-llm-control upstream_posts=2 sink_requests=2 llm_events=4 failures=0'

[private]
example-05-zcode-cli:
    #!/usr/bin/env bash
    set -euo pipefail
    PVISOR_TEST_ZCODE=1 just test-py tests/test_zcode_integration.py

[private]
example-06-tui-interception:
    #!/usr/bin/env bash
    set -euo pipefail

    example_dir="{{ repo }}/examples/pvisor/06-tui-interception"
    source "$example_dir/../common.sh"
    pvisor_example_init "$example_dir" tui-interception

    bash "$example_dir/run.sh" --once

    test "$(cat "$work_dir/workspace/private/token")" = 'original secret'
    test ! -e "$work_dir/workspace/created-by-agent.txt"
    test -e "$work_dir/stage/upper/created-by-agent.txt"
    jq -e '
      .run.state == "completed" and
      ([.run_observation.filesystem.paths["private/token"][].denied] | any(. > 0))
    ' "$work_dir/stage/run-bundle.json" >/dev/null
    jq -e '
      .network.intercepted.targets["HTTP blocked.example:80"].denied > 0
    ' "$work_dir/stage/run-bundle.json" >/dev/null

    echo 'RESULT example=tui-interception file_denied=true network_denied=true lower_unchanged=true'
