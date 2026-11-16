# S-DOCKER-001: Offline image publication is atomic on build failure

**Origin**: New Scrut contract, not one of the 88 migrated cases.

**Contract**: A local `FROM scratch` build publishes an inspectable image with
the requested tag and metadata. A subsequent invalid build under that tag fails
without replacing the image or changing the input context. Removing the tag
removes its lookup; none of these operations creates a native task.

**Prerequisites**: Executable `SUBJECT_BIN`, Python 3 and Bash. No registry,
guest execution, FUSE or hypervisor is used; this does not verify `RUN` layers.

```scrut {output_stream: combined, timeout: 70s}
$ python3 "$TESTDIR/../fixture.py" use S-DOCKER-001 --timeout 60 <<'CASE'
> test -x "$SUBJECT_BIN"
> mkdir context
> printf 'FROM scratch\nENV CONTRACT=published\nCMD ["/not-executed"]\n' > context/Dockerfile
> snapshot input context
> pvisor build -t contract:test context
> assert_unchanged input context
> pvisor image inspect contract:test > before.json
> python3 - <<'PY'
> import json
> image, = json.load(open("before.json"))
> assert image["Id"].startswith("sha256:")
> assert "CONTRACT=published" in image["Config"]["Env"]
> PY
> printf 'NOT_AN_INSTRUCTION invalid\n' > context/Dockerfile
> snapshot invalid_input context
> expect_refused pvisor build -t contract:test context
> assert_unchanged invalid_input context
> pvisor image inspect contract:test > after.json
> cmp before.json after.json
> test -z "$(pvisor ps -aq)"
> pvisor image rm contract:test
> expect_refused pvisor image inspect contract:test
> test -z "$(pvisor images -q)"
> test -z "$(find "$PVISOR_RUN_HOME" -mindepth 1 -print)"
> CASE
PASS S-DOCKER-001
```
