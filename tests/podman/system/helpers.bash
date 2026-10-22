# Adapted from Podman test/system/helpers.bash; see ../UPSTREAM.md (Apache-2.0).
bats_require_minimum_version 1.5.0

setup() {
    export PVISOR_DOCKER_HOME="$BATS_TEST_TMPDIR/store"
    NAME="pv-contract-$(python3 -c 'import uuid; print(uuid.uuid4().hex)')"
    IMAGE="${PVISOR_TEST_IMAGE:-docker.io/library/alpine:3.20}"
    ADAPTER="$BATS_TEST_DIRNAME/../command.py"
    run_cli 0 pull "$IMAGE"
}

teardown() {
    # Remove only this case's container in its private pVisor store.
    run python3 "$ADAPTER" rm -f "$NAME"
    if [[ "$status" != 0 && "$output" != *"No such container"* && "$output" != *"no such container"* ]]; then
        printf 'cleanup failed: %s\n' "$output" >&2
        return 1
    fi
}

run_cli() {
    local expected="$1"
    shift
    if ! run "-$expected" python3 "$ADAPTER" "$@"; then
        printf 'command: %s\nexpected exit %s; got %s\n%s\n' "$*" "$expected" "$status" "$output" >&2
        return 1
    fi
}
