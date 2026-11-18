# S-LAZY-009: Prepared commands and legacy adapters share one publication

**Origin**: New unreviewed compatibility-stage contract for [#161](https://github.com/DeepLink-org/pvisor/issues/161).

**Contract**: Root/grouped push publish the same prepared revision. Explicit and
configured-registry imports preserve native/E2B template fields without reading
data. Queries stay local. Both legacy spellings retain JSON stdout and warn on
stderr. This case does not assert final retirement or approve specifications.

**Prerequisites**: Executable `SUBJECT_BIN`, matching sibling `pvisor-daemon`,
Python 3, Bash and loopback sockets. The prepared fixture uses an actual native
FROM scratch build; it does not run prepare-image, a VM, execd or envd.

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-LAZY-009 --timeout 180 <<'CASE'
> source "$PVISOR_PROJECT_ROOT/tests/spec/lazy-image/regression/fixtures.bash"
> lazy_require_daemon
> mkdir context
> printf 'template bytes\n' > context/payload
> printf 'FROM scratch\nCOPY payload /payload\nLABEL role=template\n' > context/Dockerfile
> pvisor build -t lazy:template context
> publisher="$CASE_ROOT/publisher"
> python3 - "$PVISOR_DOCKER_HOME" "$publisher" <<'PY'
> import json, sys
> from pathlib import Path
> store, catalog = map(Path, sys.argv[1:])
> image, = json.loads((store / 'images.json').read_text())
> catalog.mkdir(mode=0o700)
> template = dict(rootfs=str(store), entrypoint=[], cmd=['/bin/sleep', 'infinity'], env={'PROFILE': 'fixture'}, library_dir=None, pooled_image=image, native_guest=True)
> (catalog / 'native.v1.json').write_text(json.dumps(template))
> template['e2b'] = dict(cpu_millis=1500, memory_bytes=768 * 1024 * 1024, envd_version='0.10.2')
> (catalog / 'e2b.json').write_text(json.dumps(template))
> PY
> for name in native.v1 e2b; do
>   pvisor push "$name" --images-dir "$publisher" --to "file://$CASE_ROOT/releases" > "$name.json"
>   pvisor image push "$name" --images-dir "$publisher" --to "file://$CASE_ROOT/releases" > repeated.json
>   cmp "$name.json" repeated.json
> done
> "$LAZY_DAEMON_BIN" image native.v1 --images-dir "$publisher" > legacy.json 2> direct.stderr
> pvisor daemon image native.v1 --images-dir "$publisher" > forwarded.json 2> forwarded.stderr
> cmp legacy.json forwarded.json
> for entry in direct forwarded; do grep -F 'deprecated' "$entry.stderr"; grep -F 'pvisor image' "$entry.stderr"; done
> python3 - <<'PY'
> import json
> legacy, published = [json.load(open(p)) for p in ('legacy.json', 'native.v1.json')]
> assert legacy['image'] == 'native.v1' and legacy['visibility'] == 'public'
> assert legacy['source'] == published['source'] and legacy['cid'] is None
> PY
> lazy_start_repository "$CASE_ROOT/releases"
> receiver="$CASE_ROOT/receiver"
> source_uri=$(journey_json e2b.json /source)
> pvisor pull e2b --images-dir "$receiver" --from "$LAZY_REPOSITORY/${source_uri##*/}"
> "$LAZY_DAEMON_BIN" registry --images-dir "$receiver" add origin "$LAZY_REPOSITORY"
> pvisor image pull native.v1 --images-dir "$receiver"
> pvisor image ls --images-dir "$receiver" --format '{{.Repository}}' > names
> printf 'e2b\nnative.v1\n' > expected-names
> cmp names expected-names
> pvisor image inspect native.v1 e2b --images-dir "$receiver" > inspected.json
> python3 - "$publisher" "$receiver" "$CASE_ROOT/requests.jsonl" <<'PY'
> import json, sys
> from pathlib import Path
> publisher, receiver = map(Path, sys.argv[1:3])
> for name in ('native.v1', 'e2b'):
>     before, after = [json.loads((root / f'{name}.json').read_text()) for root in (publisher, receiver)]
>     for key in ('native_guest', 'native_e2b', 'native_opensandbox', 'e2b', 'entrypoint', 'cmd', 'env'):
>         assert before.get(key) == after.get(key), (name, key)
>     for key in ('id', 'digest', 'platform'):
>         assert before['pooled_image'][key] == after['pooled_image'][key], (name, key)
>     assert before['pooled_image']['cache']['handle'] == after['pooled_image']['cache']['handle']
>     assert before['rootfs'] != after['rootfs']
> rows = json.load(open('inspected.json'))
> assert len(rows) == 2 and all(row['Pvisor']['Source'].startswith('http://127.0.0.1:') for row in rows)
> assert all(row['Config']['Labels']['role'] == 'template' for row in rows)
> assert not any('/data/' in json.loads(line)['path'] for line in open(sys.argv[3]))
> PY
> cp "$CASE_ROOT/requests.jsonl" requests-before.jsonl
> expect_refused pvisor image inspect missing --images-dir "$CASE_ROOT/absent"
> pvisor image ls --images-dir "$CASE_ROOT/absent"
> assert_absent "$CASE_ROOT/absent"
> cmp requests-before.jsonl "$CASE_ROOT/requests.jsonl"
> assert_absent "$PVISOR_DAEMON_HOME"
> test -z "$(find "$PVISOR_RUN_HOME" -mindepth 1 -print)"
> CASE
PASS S-LAZY-009
```
