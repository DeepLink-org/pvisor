#!/usr/bin/env bash
set -euo pipefail

mode="${1:?expected setup or run}"
suite="${2:?expected suite}"
variant="${PVISOR_CI_VARIANT:-}"
: "${RUNNER_TEMP:?set RUNNER_TEMP for this job}"
runtime="$HOME/pvisor-workspace/runtime"
evidence="$RUNNER_TEMP/evidence"
mkdir -p "$evidence"

install_packages() {
    sudo apt-get -o APT::Update::Error-Mode=any -o Acquire::Retries=3 update
    sudo apt-get -o Acquire::Retries=3 install -y --no-install-recommends "$@"
}

setup() {
    local scope=none tools=() packages=()
    case "$suite" in
        feature-inventory) return ;;
        agent-plugins) ;;
        spec-fixtures) tools=(python rust jq github:nextest-rs/nextest) ;;
        feature-cases)
            tools=(python rust jq github:nextest-rs/nextest)
            packages=(fuse3 util-linux crun runc)
            ;;
        firmware)
            scope=python
            tools=(python uv)
            packages=(build-essential)
            ;;
        python)
            scope=python
            tools=(python uv ruff jq rust github:nextest-rs/nextest)
            packages=(build-essential)
            ;;
        checks|rust-test)
            scope=full
            tools=(python uv rust jq github:nextest-rs/nextest)
            if [[ "$suite" == checks ]]; then tools+=(actionlint); fi
            packages=(build-essential)
            ;;
        podman-contracts)
            tools=(python)
            if [[ "$variant" == bats-* ]]; then packages=(bats); fi
            ;;
        cli-test|examples|feature-native|gateway-features|linux-product|native-components|pvisor-isolation-regression|rust-vm)
            scope=full
            tools=(python uv rust jq zig cargo:cargo-zigbuild github:nextest-rs/nextest)
            packages=(build-essential flex bison libelf-dev libssl-dev bc)
            case "$suite" in
                cli-test|gateway-features|pvisor-isolation-regression|rust-vm) packages+=(fuse3 util-linux) ;;
            esac
            ;;
        *) echo "unknown suite: $suite" >&2; exit 2 ;;
    esac

    if (( ${#packages[@]} )); then install_packages "${packages[@]}"; fi
    curl -fsSL --retry 3 https://mise.run | MISE_VERSION=v2026.10.18 sh
    export PATH="$HOME/.local/bin:$PATH"
    printf 'export PATH="$HOME/.local/bin:$PATH"\n' >> "$BASH_ENV"
    mise trust
    if (( ${#tools[@]} )); then mise install --locked "${tools[@]}"; fi
    if [[ "$scope" != none ]]; then mise run setup -- "$scope"; fi

    local extra_tool=""
    case "$suite" in
        agent-plugins) extra_tool="node@$variant" ;;
        cli-test|python) extra_tool=node@24 ;;
        podman-contracts)
            if [[ "$variant" == go-* ]]; then extra_tool=go@1.23.12; fi
            ;;
    esac
    if [[ -n "$extra_tool" ]]; then
        mise install "$extra_tool"
        printf 'export PATH=%q:"$PATH"\n' "$(mise where "$extra_tool")/bin" >> "$BASH_ENV"
    fi
}

require_userns() {
    if sysctl kernel.unprivileged_userns_clone >/dev/null 2>&1; then
        sudo sysctl -w kernel.unprivileged_userns_clone=1
    fi
    if sysctl kernel.apparmor_restrict_unprivileged_userns >/dev/null 2>&1; then
        sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
    fi
    unshare --user --map-root-user --mount --net /bin/true
}

require_fuse() {
    if [[ ! -c /dev/fuse || ! -r /dev/fuse || ! -w /dev/fuse ]]; then
        echo "this suite requires a readable and writable /dev/fuse" >&2
        return 1
    fi
    command -v fusermount3
}

require_kvm() {
    if [[ ! -c /dev/kvm ]]; then
        echo "this suite requires /dev/kvm on a nested-virtualization runner" >&2
        return 1
    fi
    sudo chown "$(id -u):$(id -g)" /dev/kvm
    /usr/bin/python3 - <<'PY'
import fcntl
import os

with open("/dev/kvm", "r+b", buffering=0) as kvm:
    if fcntl.ioctl(kvm, 0xAE00) != 12:  # KVM_GET_API_VERSION
        raise SystemExit("unsupported KVM API")
    os.close(fcntl.ioctl(kvm, 0xAE01, 0))  # KVM_CREATE_VM
PY
}

archive_runtime() {
    mise run smoke
    local firmware
    firmware="$(mise run fw -- build --target x86_64-unknown-linux-musl)"
    mkdir -p "$runtime"
    cp target/debug/{pvisor,pvisor-replay,pvisor-daemon,libkrunfw.SOURCE} "$runtime/"
    install -m 644 "$firmware" "$runtime/libkrunfw.so.5"
}

run_feature() (
    local entry="$1" feature task
    feature="$(jq -r .feature <<< "$entry")"
    export RUNNER_TEMP="$RUNNER_TEMP/$feature"
    export PVISOR_CASE_KEEP=1 PVISOR_RUN_HOME="$RUNNER_TEMP/runs"
    PVISOR_CASE_ARTIFACTS="$(mktemp -d /tmp/pvisor-feature.XXXXXX)"
    export PVISOR_CASE_ARTIFACTS
    mkdir -p "$RUNNER_TEMP/feature-evidence"
    printf 'feature=%s\ngate=%s\ncommit=%s\n' "$feature" "$CASE_GATE" "$CIRCLE_SHA1"
    if jq -e .userns <<< "$entry" >/dev/null; then require_userns; fi
    if jq -e .fuse <<< "$entry" >/dev/null; then require_fuse; fi
    if jq -e .kvm <<< "$entry" >/dev/null; then
        require_kvm
        test -s "$runtime/libkrunfw.so.5"
        export PVISOR_CASE_ROOTFS=/
    fi
    if jq -e '.vm_sdk or .feature == "lazy-image"' <<< "$entry" >/dev/null; then
        mise exec -- rustup target add x86_64-unknown-linux-musl
    fi
    if jq -e .vm_sdk <<< "$entry" >/dev/null; then
        export PVISOR_CASE_VM_PYTHON=/usr/bin/python3
        export PVISOR_CASE_IMAGE=mirror.gcr.io/library/ubuntu:24.04
        export PVISOR_CASE_AGENT=/usr/bin/true
        export PVISOR_CASE_VM_LIBRARY_DIR="$runtime"
    fi
    if [[ "$feature" == replay ]]; then test -x "$runtime/pvisor-replay"; fi
    if jq -e .oci <<< "$entry" >/dev/null; then
        crun --version
        runc --version
        export PVISOR_CASE_CONTAINER_RUNTIME=crun
        export PVISOR_CASE_CONTAINER_IMAGE=mirror.gcr.io/library/ubuntu:24.04
    fi
    task=cases-feature
    if [[ "$CASE_GATE" == strict ]]; then task=cases-feature-strict; fi
    if mise run "$task" -- "$feature" --subject-bin "$runtime/pvisor" \
        --output "$RUNNER_TEMP/feature-evidence/report.json" \
        2>&1 | tee "$RUNNER_TEMP/feature-evidence/task.log"; then
        cp "$RUNNER_TEMP/feature-evidence/report.json" "$evidence/$feature.json"
    else
        mise run archive-feature-evidence
        mv "$RUNNER_TEMP/feature-evidence.tar.gz" "$evidence/$feature.tar.gz"
        return 1
    fi
)

run_features() {
    local matrix selected entry status=0
    matrix="$(python3 scripts/tasks/feature-matrix.py)"
    selected="$(jq -c --argjson index "$CIRCLE_NODE_INDEX" --argjson total "$CIRCLE_NODE_TOTAL" \
        '.include | to_entries[] | select(.key % $total == $index) | .value' <<< "$matrix")"
    [[ -n "$selected" ]] || return 0
    while IFS= read -r entry; do
        # A failed case must not stop the remaining features on this shard.
        bash "$0" feature "$entry" </dev/null || status=1
    done <<< "$selected"
    return "$status"
}

run_vm() {
    require_userns
    require_fuse
    require_kvm
    mise run smoke

    local api_firmware native_bundle kernel status=0
    local junit=target/nextest/vm-lifecycle/junit.xml
    mise exec -- rustup target add x86_64-unknown-linux-musl
    mise exec -- rustc --edition=2021 --target x86_64-unknown-linux-musl -C linker=rust-lld \
        tests/vm_guest.rs -o "$RUNNER_TEMP/vm-guest"
    api_firmware="$(mise run fw -- build --target x86_64-unknown-linux-musl)"
    mise exec -- cargo build --locked -p pvisor-vm --example vm_api_case
    python3 tests/vm_api_contract.py --binary target/debug/examples/vm_api_case \
        --guest "$RUNNER_TEMP/vm-guest" --firmware "$api_firmware" --output "$evidence/vm-api"

    native_bundle="$(mise run kernel -- build)"
    export PVISOR_BUILD_NATIVE_KERNEL_BUNDLE="$native_bundle"
    mise exec -- cargo build --locked -p pvisor-vm --example vm_api_case
    mise exec -- rustc --edition=2021 --target x86_64-unknown-linux-musl -C linker=rust-lld \
        pvisor-kernel/tests/guest.rs -o "$RUNNER_TEMP/native-stdio-guest"
    python3 pvisor-kernel/tests/task_stdio.py --binary target/debug/examples/vm_api_case \
        --guest "$RUNNER_TEMP/native-stdio-guest"

    for kernel in linux rust-native; do
        rm -f "$junit"
        (
            export PVISOR_GUEST_KERNEL="$kernel"
            if [[ "$kernel" == linux ]]; then
                export PVISOR_CASE_VM_FIRMWARE_PATH="$api_firmware"
            else
                unset PVISOR_CASE_VM_FIRMWARE_PATH
            fi
            mise run test-vm-lifecycle
        ) 2>&1 | tee "$evidence/vm-lifecycle-$kernel.log" || status=1
        if [[ -f "$junit" ]]; then
            cp "$junit" "$evidence/vm-lifecycle-$kernel.xml"
        else
            status=1
        fi
    done
    (( status == 0 ))
    mise run test-rust -- pvisor pvisor-vm
}

run_podman() (
    require_kvm
    local shard
    case "$variant" in
        *-rust-native) shard="${variant%-rust-native}"; export PVISOR_GUEST_KERNEL=rust-native ;;
        *-linux) shard="${variant%-linux}"; export PVISOR_GUEST_KERNEL=linux ;;
        *) echo "unknown Podman variant: $variant" >&2; return 2 ;;
    esac
    export PVISOR_BINARY="$runtime/pvisor" PVISOR_TEST_IMAGE=mirror.gcr.io/library/alpine:3.20
    export PVISOR_KRUN_LOG=1 RUST_LOG=error CONTRACT_LOGS="$evidence/podman"
    local status=0
    mise run test-feature-contracts -- docker-cli "$shard" || status=$?
    if (( status )); then
        { uname -a; lscpu; sudo dmesg --ctime; } > "$evidence/host-diagnostics.txt" 2>&1 || true
    fi
    return "$status"
)

run() {
    case "$suite" in
        linux-product) archive_runtime ;;
        feature-inventory) python3 scripts/tasks/feature-matrix.py ;;
        feature-cases) run_features ;;
        feature-native)
            mise run smoke
            mise run test-feature-native -- "$variant" 2>&1 | tee "$evidence/task.log"
            ;;
        agent-plugins)
            npm test --prefix plugins
            npm ci --prefix plugins/test/hosts --ignore-scripts --legacy-peer-deps --no-audit --no-fund
            npm run test:hosts --prefix plugins -- \
                "$PWD/plugins/test/hosts/node_modules/@earendil-works/pi-coding-agent" \
                "$PWD/plugins/test/hosts/node_modules/@deepseek-ai/dsh-tools"
            ;;
        checks)
            mise run lint-workflows
            mise run fmt-rust -- --check
            mise run lint-rust
            mise run lint-api-docs
            ;;
        spec-fixtures) mise run test-spec-fixtures ;;
        python)
            mise tasks validate
            mise run fmt-py -- --check
            mise run lint-py
            mise run test-scripts
            mise run test-py-unit
            mise run test-py-regression
            mise run test-benchmark-tools
            mise run benchmark -- --smoke --criterion-output "$RUNNER_TEMP/.data/criterion-contracts"
            mise run test-home-demo
            mise run test-py -- discover -s examples/pvisor/08-compose-apps -p 'test_*.py' -v
            ;;
        firmware) mise run fw -- test ;;
        native-components)
            mise run build -- debug "$variant"
            case "$variant" in
                daemon) mise run test -- pvisor-daemon ;;
                shim) mise run test -- pvisor-shim ;;
                shim-vm) mise run test -- pvisor-shim -- --features vm ;;
                *) echo "unknown native component: $variant" >&2; exit 2 ;;
            esac
            ;;
        rust-test)
            case "$variant" in
                core)
                    mise run test -- pvisor-journal
                    mise run test-doc
                    mise run test-rust -- pvisor-journal pvisor-guest pvisor-containers pvisor-core \
                        pvisor-overlay-core pvisor-overlayfs pvisor-overlaynet pvisor-replay
                    ;;
                gateway) mise run test-rust -- pvisor-gateway ;;
                *) echo "unknown Rust shard: $variant" >&2; exit 2 ;;
            esac
            ;;
        rust-vm) run_vm ;;
        cli-test)
            require_userns
            require_fuse
            require_kvm
            PVISOR_PLUGIN_BINARY="$runtime/pvisor" PVISOR_PLUGIN_TEST_OUTPUT_DIR="$evidence/agent-plugin-cases" \
                mise run test-agent-plugins-native
            mise run test -- pvisor-cli -- -E \
                'not (binary(=docker_cli) | binary(=docker_p0_cli) | binary(=docker_staged_cli) | binary(=docker_staged_native) | binary(=compose_cli))'
            ;;
        podman-contracts) run_podman ;;
        gateway-features)
            require_userns
            require_fuse
            require_kvm
            mise run smoke
            mise run test -- pvisor pvisor-cli -- --features pvisor/gateway,pvisor-cli/gateway
            ;;
        pvisor-isolation-regression)
            require_userns
            require_fuse
            mise run build -- release
            mise run check-core-budget -- target/release/pvisor > "$evidence/pvisor-core-budget.json"
            local status=0
            PVISOR_RUN_HOME="$RUNNER_TEMP/runs" MISE_TASK_TIMINGS=true mise run test-isolation \
                2>&1 | tee "$evidence/isolation.log" || status=$?
            cp -r target/nextest/rootless "$evidence/rootless"
            (( status == 0 ))
            pkill -f "^${PWD}/target/debug/pvisor --pvisor-internal-host$" || [[ $? == 1 ]]
            WORK_ROOT="$RUNNER_TEMP/examples" mise run examples -- 01-filesystem-isolation 02-changeset-management
            ;;
        examples)
            WORK_ROOT="$RUNNER_TEMP/examples" mise run examples -- 03-network-isolation 04-gateway-llm-control
            ;;
        *) echo "unknown suite: $suite" >&2; exit 2 ;;
    esac
}

case "$mode" in
    setup) setup ;;
    run) run ;;
    feature) run_feature "$suite" ;;
    *) echo "unknown mode: $mode" >&2; exit 2 ;;
esac
