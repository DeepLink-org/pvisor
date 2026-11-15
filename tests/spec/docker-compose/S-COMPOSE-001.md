# S-COMPOSE-001: Config normalization is read-only and round-trips literal dollars

**Origin**: New Scrut contract, not a migrated case.

**Contract**: Explicit project selection overrides the file name. Service
discovery is deterministic. Exporting a normalized config and reloading it
preserves literal guest dollar expressions; config and quiet validation do not
create image/container/session stores or modify the input.

**Prerequisites**: Executable `SUBJECT_BIN`, Bash and Python 3. No VM, registry,
Compose executable or PyYAML is needed. This verifies config, not guest execution.

```scrut {output_stream: combined, timeout: 40s}
$ python3 "$TESTDIR/../fixture.py" use S-COMPOSE-001 --timeout 30 <<'CASE'
> test -x "$SUBJECT_BIN"
> mkdir project
> cat > project/compose.yaml <<'YAML'
> name: ignored
> services:
>   web:
>     image: alpine:3.20
>     command: [sh, -c, 'printf "%s" "$$HOME"']
>     environment:
>       LITERAL: "$${HOME}"
>     depends_on: [db]
>   db:
>     image: redis:7
> YAML
> snapshot input project
> pvisor compose -f project/compose.yaml -p selected config > resolved.yaml
> grep -Fx 'name: selected' resolved.yaml
> grep -F '$$HOME' resolved.yaml
> grep -F '$${HOME}' resolved.yaml
> pvisor compose -f project/compose.yaml -p selected config --services > services
> printf 'db\nweb\n' > expected
> cmp expected services
> pvisor compose -f resolved.yaml -p selected config > reloaded.yaml
> cmp resolved.yaml reloaded.yaml
> pvisor compose -f resolved.yaml -p selected config -q > quiet
> test ! -s quiet
> assert_unchanged input project
> assert_absent "$PVISOR_DOCKER_HOME"
> assert_absent "$PVISOR_IMAGE_STORE"
> test -z "$(find "$PVISOR_RUN_HOME" -mindepth 1 -print)"
> CASE
PASS S-COMPOSE-001
```
