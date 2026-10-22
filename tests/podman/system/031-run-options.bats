#!/usr/bin/env bats
# Adapted from Podman test/system/030-run.bats, 160-volumes.bats and
# test/e2e/{run,run_env}_test.go (environment, entrypoint, user and workdir).
# Copyright upstream Podman contributors; Apache-2.0. See ../UPSTREAM.md.
load helpers

@test "run explicit environment overrides env-file" {
    printf '# comment\nCONTRACT=file\nEMPTY=\n' > "$BATS_TEST_TMPDIR/env"
    run_cli 0 run --name "$NAME" --rm --env-file "$BATS_TEST_TMPDIR/env" -e CONTRACT=explicit "$IMAGE" sh -c 'printf "%s:%s" "$CONTRACT" "${EMPTY-unset}"'
    [[ "$output" == explicit: ]]
}

@test "run later env-file takes precedence" {
    printf 'CONTRACT=first\nUNCHANGED=kept\n' > "$BATS_TEST_TMPDIR/first.env"
    printf 'CONTRACT=second\n' > "$BATS_TEST_TMPDIR/second.env"
    run_cli 0 run --name "$NAME" --rm --env-file "$BATS_TEST_TMPDIR/first.env" --env-file "$BATS_TEST_TMPDIR/second.env" "$IMAGE" sh -c 'printf "%s:%s" "$CONTRACT" "$UNCHANGED"'
    [[ "$output" == second:kept ]]
}

@test "run key-only environment imports the host value" {
    export PVISOR_CONTRACT_ENV='host value with spaces'
    run_cli 0 run --name "$NAME" --rm -e PVISOR_CONTRACT_ENV "$IMAGE" printenv PVISOR_CONTRACT_ENV
    [[ "$output" == 'host value with spaces' ]]
}

@test "run entrypoint receives literal command arguments" {
    run_cli 0 run --name "$NAME" --rm --entrypoint /bin/echo "$IMAGE" 'literal ; $HOME *'
    [[ "$output" == 'literal ; $HOME *' ]]
}

# Regression: DNS setup must preserve guest directory access for non-root users.
@test "run numeric user and group are honored" {
    run_cli 0 run --name "$NAME" --rm -u 1234:2345 "$IMAGE" sh -c 'printf "%s:%s" "$(id -u)" "$(id -g)"'
    [[ "$output" == 1234:2345 ]]
}

# Regression: DNS setup must preserve guest directory access for non-root users.
@test "run named user is resolved from image passwd" {
    run_cli 0 run --name "$NAME" --rm -u nobody "$IMAGE" id -un
    [[ "$output" == nobody ]]
}

@test "run creates a missing absolute working directory" {
    run_cli 0 run --name "$NAME" --rm -w /contract/new/directory "$IMAGE" pwd
    [[ "$output" == /contract/new/directory ]]
}

@test "run working directory can be inside a bind mount" {
    mkdir -p "$BATS_TEST_TMPDIR/share/subdir"
    printf mounted > "$BATS_TEST_TMPDIR/share/subdir/input"
    run_cli 0 run --name "$NAME" --rm -v "$BATS_TEST_TMPDIR/share:/share" -w /share/subdir "$IMAGE" cat input
    [[ "$output" == mounted ]]
}

@test "read-only bind mount preserves an existing file" {
    mkdir "$BATS_TEST_TMPDIR/share"
    printf retained > "$BATS_TEST_TMPDIR/share/input"
    run_cli 1 run --name "$NAME" --rm -v "$BATS_TEST_TMPDIR/share:/share:ro" "$IMAGE" rm /share/input
    [[ "$(cat "$BATS_TEST_TMPDIR/share/input")" == retained ]]
}
