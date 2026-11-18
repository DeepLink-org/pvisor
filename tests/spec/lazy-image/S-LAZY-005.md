# S-LAZY-005: Image push and pull preserve prepared native and E2B templates

**Origin**: New unreviewed target contract for [#161](https://github.com/DeepLink-org/pvisor/issues/161).

**Contract**: Explicit prepared-catalog selection publishes the named template
and imports it into another daemon-compatible catalog. Profile flags, commands,
environment, E2B resource/version settings and the pinned image survive; only
host-local paths are rebound. Import does not fetch data objects.

**Prerequisites**: Executable `SUBJECT_BIN`, matching sibling `pvisor-daemon`,
Python 3, Bash, OpenSSL, loopback sockets and an amd64 host. Existing prepare-image
fixes linux/amd64 even without RUN and rejects arm64 hosts; guest and execd/envd
execution is not tested.

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-LAZY-005 --timeout 180 <<'CASE'
> source "$PVISOR_PROJECT_ROOT/tests/spec/lazy-image/regression/fixtures.bash"
> lazy_require_daemon
> case "$(uname -m)" in x86_64) ;; *) echo 'prepare-image requires an amd64 host' >&2; exit 77 ;; esac
> lazy_start_registry amd64
> publisher="$CASE_ROOT/publisher"
> "$LAZY_DAEMON_BIN" prepare-image --name native --image "$LAZY_OCI" --images-dir "$publisher"
> "$LAZY_DAEMON_BIN" prepare-image --name e2b --image "$LAZY_OCI" --images-dir "$publisher" --e2b --cpu-millis 1500 --memory-mib 768
> lazy_stop_registry
> python3 - "$publisher" <<'PY'
> import json, sys
> from pathlib import Path
> root = Path(sys.argv[1])
> native, e2b = [json.loads((root / f'{name}.json').read_text()) for name in ('native', 'e2b')]
> assert native['native_guest'] and native.get('e2b') is None
> assert e2b['native_guest'] and e2b['e2b']['cpu_millis'] == 1500
> assert e2b['e2b']['memory_bytes'] == 768 * 1024 * 1024
> assert e2b['e2b']['envd_version'] == '0.10.2'
> assert native['cmd'] == e2b['cmd'] == ['/bin/sleep', 'infinity']
> PY
> for name in native e2b; do
>   pvisor image push "$name" --images-dir "$publisher" --to "file://$CASE_ROOT/releases" > "$name-publication.json"
> done
> lazy_start_repository "$CASE_ROOT/releases"
> receiver="$CASE_ROOT/receiver"
> for name in native e2b; do
>   source_uri=$(journey_json "$name-publication.json" /source)
>   pvisor image pull "$name" --from "$LAZY_REPOSITORY/${source_uri##*/}" --images-dir "$receiver"
> done
> python3 - "$publisher" "$receiver" "$CASE_ROOT/requests.jsonl" <<'PY'
> import json, sys
> from pathlib import Path
> publisher, receiver = map(Path, sys.argv[1:3])
> for name in ('native', 'e2b'):
>     before, after = [json.loads((root / f'{name}.json').read_text()) for root in (publisher, receiver)]
>     for key in ('native_guest', 'native_e2b', 'native_opensandbox', 'e2b', 'entrypoint', 'cmd', 'env'):
>         assert before.get(key) == after.get(key), (name, key)
>     for key in ('digest', 'platform'):
>         assert before['pooled_image'][key] == after['pooled_image'][key], (name, key)
>     assert before['pooled_image']['cache']['handle'] == after['pooled_image']['cache']['handle']
>     assert after['rootfs'] != before['rootfs']
>     assert after['pooled_image']['cache']['repository'].startswith('http://127.0.0.1:')
> assert not any('/data/' in json.loads(line)['path'] for line in open(sys.argv[3]))
> PY
> test -z "$(find "$PVISOR_RUN_HOME" -mindepth 1 -print)"
> assert_absent "$PVISOR_DAEMON_HOME"
> CASE
PASS S-LAZY-005
```
