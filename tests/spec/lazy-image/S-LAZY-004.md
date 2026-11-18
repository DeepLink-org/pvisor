# S-LAZY-004: Image pull imports a publication lazily and preserves failure state

**Origin**: New unreviewed target contract for [#161](https://github.com/DeepLink-org/pvisor/issues/161).

**Contract**: A locally built image is published, then imported into an
independent image catalog without fetching data objects. Ordinary image inspect
keeps its array/Id contract and exposes the pinned source in its Pvisor extension.
Duplicate import and failed refresh preserve the old binding. Later diagnostic
reads verify independent exact bytes without a registry or publisher process.

**Prerequisites**: Executable `SUBJECT_BIN`, Python 3, Bash and loopback sockets.
No RUN instruction, S3, OCI service, VM or guest execution is needed.

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-LAZY-004 --timeout 180 <<'CASE'
> source "$PVISOR_PROJECT_ROOT/tests/spec/lazy-image/regression/fixtures.bash"
> mkdir context
> python3 - <<'PY'
> from pathlib import Path
> Path('context/small').write_bytes(b'value-99')
> Path('context/large').write_bytes(bytes([42]) * (2 * 1024 * 1024 + 13))
> Path('context/Dockerfile').write_text('FROM scratch\nCOPY small /small\nCOPY large /large\n')
> PY
> pvisor build -t lazy:producer context
> pvisor image push lazy:producer --to "file://$CASE_ROOT/releases" > publication.json
> source_uri=$(journey_json publication.json /source)
> handle=$(journey_json publication.json /image_handle)
> snapshot publication "$CASE_ROOT/releases"
> lazy_start_repository "$CASE_ROOT/releases"
> remote="$LAZY_REPOSITORY/${source_uri##*/}"
> export PVISOR_DOCKER_HOME="$CASE_ROOT/reader-docker" PVISOR_IMAGE_STORE="$CASE_ROOT/reader-docker/images"
> pvisor image pull lazy:reader --from "$remote"
> python3 - "$CASE_ROOT/requests.jsonl" <<'PY'
> import json, sys
> rows = [json.loads(line) for line in open(sys.argv[1])]
> assert rows and any(r['path'].endswith('/image.json') for r in rows)
> assert not any('/data/' in r['path'] for r in rows), rows
> PY
> pvisor image inspect lazy:reader > installed.json
> python3 - "$remote" <<'PY'
> import json, sys
> original = json.load(open('publication.json'))
> installed, = json.load(open('installed.json'))
> assert installed['Id'].startswith('sha256:')
> assert installed['Pvisor']['Source'] == sys.argv[1]
> for source, target in [('image_handle', 'ImageHandle'), ('digest', 'Digest'), ('platform', 'Platform')]:
>     assert installed['Pvisor'][target] == original[source], source
> PY
> expect_refused pvisor image pull lazy:reader --from "$remote"
> touch "$CASE_ROOT/repository-deny"
> expect_refused pvisor image pull lazy:reader --from "$remote" --refresh
> pvisor image inspect lazy:reader > after-failure.json
> cmp installed.json after-failure.json
> rm "$CASE_ROOT/repository-deny"
> export PVISOR_CACHE_BACKEND=http PVISOR_CACHE_LOCATION="$remote" PVISOR_CACHE_READ_ONLY=true
> pvisor cache read "$handle" small > small
> pvisor cache read "$handle" large > large
> python3 - "$CASE_ROOT/requests.jsonl" <<'PY'
> import json, sys
> from pathlib import Path
> assert Path('small').read_bytes() == b'value-99'
> assert Path('large').read_bytes() == bytes([42]) * (2 * 1024 * 1024 + 13)
> rows = [json.loads(line) for line in open(sys.argv[1])]
> assert any('/data/' in r['path'] and r['range'] is not None for r in rows)
> PY
> assert_unchanged publication "$CASE_ROOT/releases"
> assert_absent "$PVISOR_DAEMON_HOME"
> CASE
PASS S-LAZY-004
```
