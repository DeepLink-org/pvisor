# S-LAZY-006: Retired daemon image commands refuse without side effects

**Origin**: New unreviewed final-retirement contract for [#161](https://github.com/DeepLink-org/pvisor/issues/161).

**Contract**: Direct and forwarded daemon image commands fail with a
`pvisor image` migration diagnostic before creating state.
Daemon help omits image and the IPFS service flag. The hidden image-management
entry point also refuses. Template preparation,
registry configuration and cache serving remain available.

**Prerequisites**: Executable `SUBJECT_BIN`, matching sibling `pvisor-daemon`,
Python 3 and Bash. No workload or network service is started.

```scrut {output_stream: combined, timeout: 40s}
$ python3 "$TESTDIR/../fixture.py" use S-LAZY-006 --timeout 30 <<'CASE'
> source "$PVISOR_PROJECT_ROOT/tests/spec/lazy-image/regression/fixtures.bash"
> lazy_require_daemon
> "$LAZY_DAEMON_BIN" serve --help > service-help
> grep -F -- '--cache' service-help
> for entry in direct forwarded; do
>   if [ "$entry" = direct ]; then executable=("$LAZY_DAEMON_BIN"); else executable=("$SUBJECT_BIN" daemon); fi
>   expect_refused "${executable[@]}" image missing --images-dir "$CASE_ROOT/legacy" > old.stdout 2> old.stderr
>   test ! -s old.stdout
>   grep -F 'pvisor image' old.stderr
>   assert_absent "$CASE_ROOT/legacy"
>   expect_refused "${executable[@]}" image missing --private --images-dir "$CASE_ROOT/legacy" > old.stdout 2> old.stderr
>   grep -F 'pvisor image' old.stderr
>   assert_absent "$CASE_ROOT/legacy"
>   expect_refused "${executable[@]}" image missing --public --to "file://$CASE_ROOT/old-target" --images-dir "$CASE_ROOT/legacy" > old.stdout 2> old.stderr
>   test ! -s old.stdout
>   grep -F 'pvisor image' old.stderr
>   assert_absent "$CASE_ROOT/legacy"
>   assert_absent "$CASE_ROOT/old-target"
>   expect_refused "${executable[@]}" image-manage --images-dir "$CASE_ROOT/legacy" ls > old.stdout 2> old.stderr
>   test ! -s old.stdout
>   grep -F 'unrecognized subcommand' old.stderr
>   assert_absent "$CASE_ROOT/legacy"
>   "${executable[@]}" --help > daemon-help
>   ! grep -Eq '^[[:space:]]+image[[:space:]]' daemon-help
>   "${executable[@]}" serve --help > service-help
>   grep -F -- '--cache' service-help
>   ! grep -F -- '--ipfs' service-help
>   expect_refused "${executable[@]}" serve --cache --ipfs --cache-listen "unix://$CASE_ROOT/cache.sock" > old.stdout 2> old.stderr
>   test ! -s old.stdout
>   grep -F -- '--ipfs' old.stderr
>   assert_absent "$CASE_ROOT/cache.sock"
>   "${executable[@]}" prepare-image --help
>   "${executable[@]}" registry --help
> done
> for path in "$PVISOR_IMAGE_STORE" "$PVISOR_DOCKER_HOME" "$PVISOR_DAEMON_HOME" "$WS/.pvisor"; do assert_absent "$path"; done
> test -z "$(find "$PVISOR_RUN_HOME" -mindepth 1 -print)"
> CASE
PASS S-LAZY-006
```
