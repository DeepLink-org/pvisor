# S-COMPOSE-002: Invalid dependency graphs and unsupported privilege are inert

**Origin**: New Scrut contract, not a migrated case.

**Contract**: Missing dependencies, dependency cycles and unsupported privilege
are rejected before a project can allocate stores or launch workloads. A valid
config is a positive control; an unknown selected service also fails without
mutating its file or starting a native task.

**Prerequisites**: Executable `SUBJECT_BIN` and Bash. No VM or registry is used.
This checks admission, not rollback of an already running project.

```scrut {output_stream: combined, timeout: 40s}
$ python3 "$TESTDIR/../fixture.py" use S-COMPOSE-002 --timeout 30 <<'CASE'
> test -x "$SUBJECT_BIN"
> printf 'services: {app: {image: alpine:3.20}}\n' > compose.yaml
> pvisor compose -f compose.yaml -p admission config -q
> snapshot valid .
> expect_refused pvisor compose -f compose.yaml -p admission up -d missing
> assert_unchanged valid .
> printf 'services: {app: {image: alpine, depends_on: [missing]}}\n' > compose.yaml
> expect_refused pvisor compose -f compose.yaml -p admission config -q
> printf 'services: {app: {image: alpine, depends_on: [db]}, db: {image: alpine, depends_on: [app]}}\n' > compose.yaml
> expect_refused pvisor compose -f compose.yaml -p admission config -q
> printf 'services: {app: {image: alpine, privileged: true}}\n' > compose.yaml
> expect_refused pvisor compose -f compose.yaml -p admission up -d
> assert_absent "$PVISOR_DOCKER_HOME"
> assert_absent "$PVISOR_IMAGE_STORE"
> test -z "$(find "$PVISOR_RUN_HOME" -mindepth 1 -print)"
> CASE
PASS S-COMPOSE-002
```
