#!/usr/bin/env bats
# Adapted from Podman test/system/070-build.bats; Apache-2.0. See ../UPSTREAM.md.
# Ports only the documented Docker build subset, without Podman log formatting.
load helpers

@test "build basic cached image runs generated file and can be removed" {
    mkdir "$BATS_TEST_TMPDIR/context"
    printf 'FROM %s\nRUN echo built-content > /generated\n' "$IMAGE" > "$BATS_TEST_TMPDIR/context/Dockerfile"
    run_cli 0 build --network=none -t "$NAME" "$BATS_TEST_TMPDIR/context"
    [[ "$output" != *"Pulling"* ]]
    run_cli 0 run --rm --name "$NAME" "$NAME" cat /generated
    [[ "$output" == built-content ]]
    run_cli 0 rmi "$NAME"
    run_cli 0 images -q "$NAME"
    [[ "$output" == '' ]]
}

# Source: build test -f ./relative. Preserve relative TMPDIR and definition path;
# the explicit definition is outside the supplied context.
@test "build relative Dockerfile outside context with relative TMPDIR" {
    mkdir "$BATS_TEST_TMPDIR/context" "$BATS_TEST_TMPDIR/definitions" "$BATS_TEST_TMPDIR/relative-tmp"
    printf 'FROM %s\nRUN echo relative-file > /generated\n' "$IMAGE" > "$BATS_TEST_TMPDIR/definitions/Containerfile"
    cd "$BATS_TEST_TMPDIR"
    TMPDIR=relative-tmp run_cli 0 build --network=none -t "$NAME" -f ./definitions/Containerfile ./context
    run_cli 0 run --rm --name "$NAME" "$NAME" cat /generated
    [[ "$output" == relative-file ]]
}

# Source: build with ignore '*'. Pod create and Containerfile auto-discovery are
# excluded; keep Dockerfile discovery, missing-file rejection and explicit -f.
@test "build ignore all still reads Dockerfile and explicit renamed definition" {
    mkdir "$BATS_TEST_TMPDIR/context"
    printf 'FROM scratch\n' > "$BATS_TEST_TMPDIR/context/Dockerfile"
    printf '*\n' > "$BATS_TEST_TMPDIR/context/.dockerignore"
    run_cli 0 build -t "$NAME" "$BATS_TEST_TMPDIR/context"
    mv "$BATS_TEST_TMPDIR/context/Dockerfile" "$BATS_TEST_TMPDIR/context/renamed"
    run_cli 1 build -t "$NAME" "$BATS_TEST_TMPDIR/context"
    [[ "$output" == *"cannot read Dockerfile"* ]]
    run_cli 0 build -t "$NAME" -f "$BATS_TEST_TMPDIR/context/renamed" "$BATS_TEST_TMPDIR/context"
}

# Source: build with ignore '*' and containerfile outside build context.
# Use Docker's .dockerignore rather than Podman's .containerignore.
@test "build ignore all allows explicit definition outside context" {
    mkdir "$BATS_TEST_TMPDIR/context"
    printf 'FROM scratch\n' > "$BATS_TEST_TMPDIR/Containerfile"
    printf '*\n' > "$BATS_TEST_TMPDIR/context/.dockerignore"
    run_cli 0 build -t "$NAME" -f "$BATS_TEST_TMPDIR/Containerfile" "$BATS_TEST_TMPDIR/context"
    run_cli 0 images -q "$NAME"
    [[ "$output" != '' ]]
}

# Source: build context is a symlink to a directory. Add a runtime assertion to
# the upstream successful-build assertion, which has a FIXME about observation.
@test "build context can be a symlink to a directory" {
    mkdir "$BATS_TEST_TMPDIR/target"
    ln -s target "$BATS_TEST_TMPDIR/link"
    printf 'FROM %s\nRUN echo hello > /generated\n' "$IMAGE" > "$BATS_TEST_TMPDIR/target/Dockerfile"
    run_cli 0 build --network=none -t "$NAME" "$BATS_TEST_TMPDIR/link"
    run_cli 0 run --rm --name "$NAME" "$NAME" cat /generated
    [[ "$output" == hello ]]
}

# Source: .dockerignore failure test. Docker-profile build errors use exit 1;
# keep failure at COPY and assert that no output tag is published.
@test "build COPY of ignored source fails without publishing a tag" {
    mkdir -p "$BATS_TEST_TMPDIR/context/subdir"
    printf 'FROM %s\nCOPY ./ ./\nCOPY subdir ./\n' "$IMAGE" > "$BATS_TEST_TMPDIR/context/Dockerfile"
    printf '*\nsubdir\n!*/sub1*\n' > "$BATS_TEST_TMPDIR/context/.dockerignore"
    run_cli 1 build -t "$NAME" "$BATS_TEST_TMPDIR/context"
    [[ "$output" == *"(COPY)"* ]]
    run_cli 0 images -q "$NAME"
    [[ "$output" == '' ]]
}
