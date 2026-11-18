# S-LAZY-008: Push admission never guesses a destination or pulls a missing image

**Origin**: New unreviewed target contract for [#161](https://github.com/DeepLink-org/pvisor/issues/161).

**Contract**: Explicit publication rejects relative/credential-bearing or
read-only destinations. Missing ordinary images and prepared templates do not
fall back to OCI. An unsupported ordinary push cannot silently use ambient
cache configuration as an S3/filesystem destination. Root push and grouped image
push have the same refusal behavior. No target or task is made.

**Prerequisites**: Executable `SUBJECT_BIN`, Python 3 and Bash; no VM or service.
The positive help control prevents unknown-command errors from satisfying the
negative assertions on an implementation with no push entry point.

```scrut {output_stream: combined, timeout: 40s}
$ python3 "$TESTDIR/../fixture.py" use S-LAZY-008 --timeout 30 <<'CASE'
> source "$PVISOR_PROJECT_ROOT/tests/spec/lazy-image/regression/fixtures.bash"
> pvisor image push --help > push-help
> grep -F -- '--to' push-help
> grep -F -- '--images-dir' push-help
> pvisor push --help > root-push-help
> grep -F -- '--to' root-push-help
> grep -F -- '--images-dir' root-push-help
> mkdir context
> printf 'checked\n' > context/marker
> printf 'FROM scratch\nCOPY marker /marker\n' > context/Dockerfile
> pvisor build -t lazy:admission context
> image=127.0.0.1:9/missing:fixture
> export PVISOR_CACHE_BACKEND=filesystem PVISOR_CACHE_LOCATION="$CASE_ROOT/ambient"
> for entry in root grouped; do
>   if [ "$entry" = root ]; then push_command=(pvisor push); else push_command=(pvisor image push); fi
>   expect_refused "${push_command[@]}" "$image"
>   assert_absent "$CASE_ROOT/ambient"
>   for destination in file://relative http://127.0.0.1:9/readonly 's3://user:secret@bucket/prefix' 's3://bucket/prefix?token=secret' 's3://bucket/prefix#fragment'; do
>     expect_refused "${push_command[@]}" lazy:admission --to "$destination" > invalid.stdout 2> invalid.stderr
>     test ! -s invalid.stdout
>     ! grep -F 'secret' invalid.stdout invalid.stderr
>   done
>   expect_refused "${push_command[@]}" "$image" --to "file://$CASE_ROOT/output"
>   expect_refused "${push_command[@]}" missing --images-dir "$CASE_ROOT/catalog" --to "file://$CASE_ROOT/output"
> done
> for path in "$CASE_ROOT/output" "$CASE_ROOT/ambient" "$PVISOR_DAEMON_HOME"; do assert_absent "$path"; done
> test -z "$(find "$PVISOR_RUN_HOME" -mindepth 1 -print)"
> CASE
PASS S-LAZY-008
```
