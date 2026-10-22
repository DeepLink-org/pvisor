#!/usr/bin/env bats
# Adapted from Podman test/system/{030-run,160-volumes}.bats and
# test/e2e/{run,run_env}_test.go; Apache-2.0. See ../UPSTREAM.md.
load helpers

@test "run a directory returns 126" {
    run_cli 126 run --name "$NAME" --rm "$IMAGE" /etc
}

@test "run a non-executable file returns 126" {
    mkdir "$BATS_TEST_TMPDIR/share"
    printf '#!/bin/sh\necho forbidden\n' > "$BATS_TEST_TMPDIR/share/program"
    chmod 0644 "$BATS_TEST_TMPDIR/share/program"
    run_cli 126 run --name "$NAME" --rm -v "$BATS_TEST_TMPDIR/share:/share" "$IMAGE" /share/program
    [[ "$output" != *forbidden* ]]
}

@test "run --rm releases the name after failed execution" {
    run_cli 127 run --name "$NAME" --rm "$IMAGE" /no-such-command
    run_cli 0 run --name "$NAME" --rm "$IMAGE" true
}

@test "run does not implicitly inherit host environment" {
    export PVISOR_CONTRACT_SECRET='must not leak'
    run_cli 0 run --name "$NAME" --rm "$IMAGE" sh -c 'printf "%s" "${PVISOR_CONTRACT_SECRET-unset}"'
    [[ "$output" == unset ]]
}

@test "run preserves an explicitly empty environment value" {
    run_cli 0 run --name "$NAME" --rm -e CONTRACT= "$IMAGE" sh -c 'printf "<%s>" "${CONTRACT-unset}"'
    [[ "$output" == '<>' ]]
}

@test "run preserves option-like workload arguments" {
    run_cli 0 run --name "$NAME" --rm "$IMAGE" echo --name=literal --platform=literal
    [[ "$output" == '--name=literal --platform=literal' ]]
}

@test "run rejects relative working directories before creating a container" {
    run_cli 125 run --name "$NAME" --rm -w relative "$IMAGE" true
    run_cli 0 ps -aq
    [[ -z "$output" ]]
}

@test "run --network none has no ethernet device" {
    run_cli 0 run --name "$NAME" --rm --network none "$IMAGE" sh -c 'test "$(ls /sys/class/net)" = lo'
}
