# S-LAZY-007: Existing shared-pool readers outlive the publisher and reject corruption

**Origin**: New unreviewed preservation contract for [#161](https://github.com/DeepLink-org/pvisor/issues/161).

**Contract**: Existing filesystem publication remains readable by independent
read-only filesystem and HTTP clients after OCI shutdown and staging removal.
File bytes match the independent source fixture. Read-only prepare/refresh
refusals do not mutate the pool; corrupt remote data fails without emitting
unverified file bytes or repairing shared objects in place.

**Prerequisites**: The Rust cache client example, Python 3, Bash, OpenSSL and
loopback sockets. No S3, guest, FUSE or hypervisor. This guards the legacy shared-pool
contract rather than claiming new bundle/import support.

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-LAZY-007 --timeout 180 <<'CASE'
> source "$PVISOR_PROJECT_ROOT/tests/spec/lazy-image/regression/fixtures.bash"
> lazy_require_cache_client
> lazy_start_registry
> shared="$CASE_ROOT/shared"
> export PVISOR_CACHE_BACKEND=filesystem PVISOR_CACHE_LOCATION="$shared"
> lazy_cache_prepare "$LAZY_OCI" > prepared.json
> json_expect prepared.json /digest "\"$LAZY_DIGEST\""
> handle=$(journey_json prepared.json /image_handle)
> test -z "$(find "$PVISOR_IMAGE_STORE/staging" -mindepth 1 -print)"
> lazy_stop_registry
> rm -rf -- "$PVISOR_IMAGE_STORE"
> snapshot shared "$shared"
> export PVISOR_CACHE_READ_ONLY=true
> lazy_cache_prepare "$LAZY_OCI" > reopened.json
> json_expect reopened.json /image_handle "\"$handle\""
> "$PVISOR_CASE_CACHE_PROBE" read-file "$handle" small-099 > small
> lazy_start_repository "$shared"
> PVISOR_CACHE_BACKEND=http PVISOR_CACHE_LOCATION="$LAZY_REPOSITORY" PVISOR_CACHE_READ_ONLY=true "$PVISOR_CASE_CACHE_PROBE" read-file "$handle" large > large
> python3 - "$CASE_ROOT/requests.jsonl" <<'PY'
> import json, sys
> from pathlib import Path
> assert Path('small').read_bytes() == b'value-99'
> assert Path('large').read_bytes() == bytes([42]) * (2 * 1024 * 1024 + 13)
> assert any('/data/' in json.loads(line)['path'] for line in open(sys.argv[1]))
> PY
> expect_refused lazy_cache_prepare "$LAZY_OCI" true
> expect_refused lazy_cache_prepare absent:fixture
> assert_unchanged shared "$shared"
> assert_absent "$PVISOR_IMAGE_STORE"
> python3 - "$shared" <<'PY'
> import hashlib, sys
> from pathlib import Path
> digest = hashlib.sha256(bytes([42]) * 1024 * 1024).hexdigest()
> path = Path(sys.argv[1]) / 'data/sha256' / digest[:2] / digest[2:4] / digest
> assert path.is_file()
> path.write_bytes(b'corrupt remote object')
> PY
> snapshot corrupt "$shared"
> expect_refused env HOME="$CASE_ROOT/cold-home" XDG_CACHE_HOME="$CASE_ROOT/cold-cache" "$PVISOR_CASE_CACHE_PROBE" read-file "$handle" large > corrupt.stdout 2> corrupt.stderr
> test ! -s corrupt.stdout
> grep -Ei 'digest|integrity|corrupt|range' corrupt.stderr
> assert_unchanged corrupt "$shared"
> assert_absent "$PVISOR_DAEMON_HOME"
> CASE
PASS S-LAZY-007
```
