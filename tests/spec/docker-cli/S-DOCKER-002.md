# S-DOCKER-002: Invalid Docker commands cannot become host tasks

**Origin**: New Scrut contract, not a migrated case.

**Contract**: Unsupported root commands, privileged execution and invalid copy
syntax fail before storage or task creation. In particular an unknown command
must not be interpreted as a host process. Help remains usable without a store.

**Prerequisites**: Executable `SUBJECT_BIN` and Bash. No VM or network is used.
The post-failure oracle checks both namespaces, not only the exit status.

```scrut {output_stream: combined, timeout: 40s}
$ python3 "$TESTDIR/../fixture.py" use S-DOCKER-002 --timeout 30 <<'CASE'
> test -x "$SUBJECT_BIN"
> pvisor container run --help
> pvisor image inspect --help
> expect_refused pvisor touch must-not-exist
> expect_refused pvisor run --privileged unused-image
> expect_refused pvisor cp local other
> expect_refused pvisor cp one:/file two:/file
> expect_refused pvisor ps --filter unknown=value
> assert_absent must-not-exist
> assert_absent "$PVISOR_DOCKER_HOME"
> assert_absent "$PVISOR_IMAGE_STORE"
> test -z "$(find "$PVISOR_RUN_HOME" -mindepth 1 -print)"
> CASE
PASS S-DOCKER-002
```
