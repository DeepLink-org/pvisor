# S-LAZY-001: Lazy publication preserves ordinary image, build and service behavior

**Origin**: New unreviewed contract for [#161](https://github.com/DeepLink-org/pvisor/issues/161).

**Contract**: Docker image help retains OCI pull/platform syntax and build keeps
its OCI export spelling; daemon service help retains cache serving.
Help and invocations of the retired local base preparation commands
do not initialize image, container, daemon or Session state or mutate inputs.

**Prerequisites**: Matching executable `SUBJECT_BIN` and sibling `pvisor-daemon`,
Python 3 and Bash. No VM, FUSE, registry or network service is used.

```scrut {output_stream: combined, timeout: 40s}
$ python3 "$TESTDIR/../fixture.py" use S-LAZY-001 --timeout 30 <<'CASE'
> source "$PVISOR_PROJECT_ROOT/tests/spec/lazy-image/regression/fixtures.bash"
> lazy_require_daemon
> pvisor image --help > image-help
> pvisor image pull --help > pull-help
> grep -F -- '--platform' pull-help
> pvisor build --help > build-help
> grep -F -- 'type=oci,dest=PATH' build-help
> ! grep -Eq '^[[:space:]]+(prepare-image|registry|publish)[[:space:]]' image-help
> "$LAZY_DAEMON_BIN" serve --help > service-help
> grep -F -- '--cache' service-help
> pvisor daemon serve --help > forwarded-help
> cmp service-help forwarded-help
> pvisor --help > root-help
> ! grep -Eq '^[[:space:]]+cache[[:space:]]' root-help
> expect_refused pvisor cache --help
> mkdir input
> printf 'untouched\n' > input/value
> snapshot input input
> expect_refused pvisor cache prepare-rootfs --rootfs "$WS/input" --base-store "$CASE_ROOT/bases"
> expect_refused pvisor cache prepare-workspace --workspace "$WS/input" --base-store "$CASE_ROOT/bases"
> expect_refused pvisor cache --backend filesystem --location "$CASE_ROOT/shared" prepare-rootfs --rootfs "$WS/input" --base-store "$CASE_ROOT/bases"
> expect_refused pvisor cache --read-only prepare-workspace --workspace "$WS/input" --base-store "$CASE_ROOT/bases"
> assert_unchanged input input
> for path in "$CASE_ROOT/shared" "$CASE_ROOT/bases" "$PVISOR_IMAGE_STORE" "$PVISOR_DOCKER_HOME" "$PVISOR_DAEMON_HOME"; do assert_absent "$path"; done
> test -z "$(find "$PVISOR_RUN_HOME" -mindepth 1 -print)"
> CASE
PASS S-LAZY-001
```
