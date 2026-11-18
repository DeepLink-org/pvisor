# S-LAZY-003: Both push entry points publish the same local revision

**Origin**: New unreviewed target contract for [#161](https://github.com/DeepLink-org/pvisor/issues/161).

**Contract**: A locally built image needs no OCI source to publish. Explicit
destination selection overrides ambient cache configuration. Publication returns
a fixed complete bundle, is idempotent, and preserves local image identity and
configuration. This also checks the no-RUN build prerequisite with a real image.
Root push and grouped image push produce the same publication receipt.

**Prerequisites**: Executable `SUBJECT_BIN`, Python 3 and Bash. No registry, VM,
S3 or other network service is required.

```scrut {output_stream: combined, timeout: 70s}
$ python3 "$TESTDIR/../fixture.py" use S-LAZY-003 --timeout 60 <<'CASE'
> source "$PVISOR_PROJECT_ROOT/tests/spec/lazy-image/regression/fixtures.bash"
> mkdir context
> printf 'source bytes\n' > context/hello
> printf 'FROM scratch\nCOPY hello /hello\nENV CONTRACT=local\nCMD ["/not-executed"]\n' > context/Dockerfile
> pvisor image build -t lazy:local context
> pvisor image inspect lazy:local > before.json
> export PVISOR_CACHE_BACKEND=filesystem PVISOR_CACHE_LOCATION="$CASE_ROOT/ambient-target"
> pvisor push lazy:local --to "file://$CASE_ROOT/releases" > published.json
> assert_absent "$CASE_ROOT/ambient-target"
> pvisor image push lazy:local --to "file://$CASE_ROOT/releases" > repeated.json
> pvisor image inspect lazy:local > after.json
> python3 - "$CASE_ROOT/releases" <<'PY'
> import json, re, sys
> from pathlib import Path
> from urllib.parse import urlsplit
> first, second = [json.load(open(p)) for p in ('published.json', 'repeated.json')]
> for key in ('source', 'image_handle', 'digest', 'platform'): assert first[key] == second[key], key
> assert first['image_handle'].startswith('pvisor-v2:')
> assert re.fullmatch('sha256:[0-9a-f]{64}', first['digest'])
> assert first['platform'] in ('linux/amd64', 'linux/arm64')
> source = urlsplit(first['source'])
> assert source.scheme == 'file' and re.fullmatch('[0-9a-f]{64}', Path(source.path).name)
> assert Path(source.path).parent == Path(sys.argv[1])
> assert (Path(source.path) / 'image.json').is_file()
> before, after = [json.load(open(p))[0] for p in ('before.json', 'after.json')]
> for key in ('Id', 'Config', 'Architecture', 'Os', 'RepoTags'): assert before[key] == after[key], key
> PY
> test -z "$(pvisor ps -aq)"
> test -z "$(find "$PVISOR_RUN_HOME" -mindepth 1 -print)"
> assert_absent "$PVISOR_DAEMON_HOME"
> CASE
PASS S-LAZY-003
```
