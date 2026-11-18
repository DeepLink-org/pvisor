# S-LAZY-002: Lazy publication extends image commands without a duplicate cache tree

**Origin**: New unreviewed target contract for [#161](https://github.com/DeepLink-org/pvisor/issues/161).

**Contract**: Root and image help expose equivalent push entry points and their
explicit destination, pull exposes
explicit publication import, and catalog queries accept prepared-catalog
selection. No new cache pull/inspect/registry command tree is introduced.
Help leaves all stores absent and starts no service.

**Prerequisites**: Executable `SUBJECT_BIN`, Python 3 and Bash; no network or VM.

```scrut {output_stream: combined, timeout: 40s}
$ python3 "$TESTDIR/../fixture.py" use S-LAZY-002 --timeout 30 <<'CASE'
> source "$PVISOR_PROJECT_ROOT/tests/spec/lazy-image/regression/fixtures.bash"
> pvisor image --help > image-help
> for operation in build pull push ls inspect rm; do grep -Eq "^[[:space:]]+$operation([[:space:]]|$)" image-help; done
> pvisor image push --help > push-help
> for option in --to --images-dir; do grep -F -- "$option" push-help; done
> pvisor --help > root-help
> grep -Eq '^[[:space:]]+push([[:space:]]|$)' root-help
> pvisor push --help > root-push-help
> for option in --to --images-dir; do grep -F -- "$option" root-push-help; done
> pvisor image pull --help > pull-help
> for option in --from --refresh --images-dir; do grep -F -- "$option" pull-help; done
> pvisor image inspect --help > inspect-help
> grep -F -- '--images-dir' inspect-help
> pvisor image ls --help > list-help
> grep -F -- '--images-dir' list-help
> pvisor cache --help > cache-help
> ! grep -Eq '^[[:space:]]+(pull|inspect|registry)[[:space:]]' cache-help
> for path in "$PVISOR_IMAGE_STORE" "$PVISOR_DOCKER_HOME" "$PVISOR_DAEMON_HOME" "$WS/.pvisor"; do assert_absent "$path"; done
> test -z "$(find "$PVISOR_RUN_HOME" -mindepth 1 -print)"
> CASE
PASS S-LAZY-002
```
