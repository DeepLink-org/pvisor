#!/usr/bin/env bats
# Adapted from Podman test/e2e/run_env_test.go and test/system/160-volumes.bats.
# Copyright upstream Podman contributors; Apache-2.0. See ../UPSTREAM.md.
load helpers

@test "run later repeated environment takes precedence with literal values" {
    run_cli 0 run --rm --name "$NAME" -e CONTRACT=first -e 'CONTRACT=last=with, delimiters $HOME' "$IMAGE" printenv CONTRACT
    [[ "$output" == 'last=with, delimiters $HOME' ]]
}

@test "run key-only missing environment removes a previous value" {
    unset PVISOR_CONTRACT_MISSING
    run_cli 1 run --rm --name "$NAME" -e PVISOR_CONTRACT_MISSING=earlier -e PVISOR_CONTRACT_MISSING "$IMAGE" printenv PVISOR_CONTRACT_MISSING
    [[ "$output" == '' ]]
}

@test "run env-file accepts CRLF and preserves embedded equals" {
    printf '# comment\r\nCONTRACT=value=with=equals\r\nEMPTY=\r\n' > "$BATS_TEST_TMPDIR/env"
    run_cli 0 run --rm --name "$NAME" --env-file "$BATS_TEST_TMPDIR/env" "$IMAGE" sh -c 'printf "%s:%s" "$CONTRACT" "${EMPTY-unset}"'
    [[ "$output" == value=with=equals: ]]
}

@test "run rejects a missing bind source without publishing a container" {
    run_cli 125 run --name "$NAME" -v "$BATS_TEST_TMPDIR/missing:/data" "$IMAGE" true
    [[ ! -e "$BATS_TEST_TMPDIR/missing" ]]
    run_cli 0 ps -aq
    [[ "$output" == '' ]]
}

@test "run rejects conflicting volume destinations before execution" {
    mkdir "$BATS_TEST_TMPDIR/first" "$BATS_TEST_TMPDIR/second"
    run_cli 125 run --name "$NAME" -v "$BATS_TEST_TMPDIR/first:/data" -v "$BATS_TEST_TMPDIR/second:/data" "$IMAGE" sh -c 'echo unexpected > /data/value'
    [[ ! -e "$BATS_TEST_TMPDIR/first/value" && ! -e "$BATS_TEST_TMPDIR/second/value" ]]
    run_cli 0 ps -aq
    [[ "$output" == '' ]]
}

@test "run bind paths with spaces preserve literal filenames" {
    mkdir "$BATS_TEST_TMPDIR/a share"
    printf literal > "$BATS_TEST_TMPDIR/a share/a file"
    run_cli 0 run --rm --name "$NAME" -v "$BATS_TEST_TMPDIR/a share:/a share" "$IMAGE" sh -c 'cat "/a share/a file"; printf updated > "/a share/a file"'
    [[ "$output" == literal ]]
    [[ "$(cat "$BATS_TEST_TMPDIR/a share/a file")" == updated ]]
}
