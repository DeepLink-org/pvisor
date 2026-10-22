#!/usr/bin/env bats
# Adapted from Podman test/system/030-run.bats and 160-volumes.bats.
# Copyright upstream Podman contributors; Apache-2.0. See ../UPSTREAM.md.
load helpers

@test "run returns the workload exit code" {
    run_cli 7 run --name "$NAME" --rm "$IMAGE" sh -c 'exit 7'
}

@test "run missing executable returns 127" {
    run_cli 127 run --name "$NAME" --rm "$IMAGE" /no-such-command
}

@test "run -i transfers stdin and EOF" {
    run_cli 0 run --name "$NAME" --rm -i "$IMAGE" cat <<< 'hello stdin'
    [[ "$output" == 'hello stdin' ]]
}

@test "run applies environment and working directory" {
    run_cli 0 run --name "$NAME" --rm -e CONTRACT=value -w /tmp "$IMAGE" sh -c 'printf "%s:%s" "$CONTRACT" "$PWD"'
    [[ "$output" == 'value:/tmp' ]]
}

@test "bind mount reads and writes host files" {
    mkdir "$BATS_TEST_TMPDIR/share"
    printf input > "$BATS_TEST_TMPDIR/share/input"
    run_cli 0 run --name "$NAME" --rm -v "$BATS_TEST_TMPDIR/share:/share" "$IMAGE" sh -c 'cat /share/input; printf result > /share/output'
    [[ "$output" == input ]]
    [[ "$(cat "$BATS_TEST_TMPDIR/share/output")" == result ]]
}

@test "read-only bind mount rejects mutation" {
    mkdir "$BATS_TEST_TMPDIR/share"
    run_cli 1 run --name "$NAME" --rm -v "$BATS_TEST_TMPDIR/share:/share:ro" "$IMAGE" touch /share/forbidden
    [[ ! -e "$BATS_TEST_TMPDIR/share/forbidden" ]]
}
