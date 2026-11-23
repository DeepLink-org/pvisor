# S-LAZY-007: Existing shared-pool readers outlive the publisher and reject corruption

**Origin**: New unreviewed preservation contract for [#161](https://github.com/DeepLink-org/pvisor/issues/161).

**Contract**: Existing filesystem publication remains readable by independent
read-only filesystem and HTTP clients after OCI shutdown and staging removal.
File bytes match the independent source fixture. Read-only prepare/refresh
refusals do not mutate the pool. Seeded preparation works when blob downloads
are denied and rejects missing or corrupt local input. Corrupt remote data fails
without emitting
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
> seeds="$CASE_ROOT/registry/blobs"
> snapshot seeds "$seeds"
> touch "$CASE_ROOT/registry/deny-blobs"
> PVISOR_CACHE_LOCATION="$CASE_ROOT/seeded" "$PVISOR_CASE_CACHE_PROBE" prepare-with-blobs "$LAZY_OCI" "${LAZY_PLATFORM#linux/}" "$seeds" > seeded.json
> json_expect seeded.json /digest "\"$LAZY_DIGEST\""
> assert_unchanged seeds "$seeds"
> rm "$CASE_ROOT/registry/deny-blobs"
> mkdir "$CASE_ROOT/empty-blobs"
> expect_refused env PVISOR_CACHE_LOCATION="$CASE_ROOT/incomplete" "$PVISOR_CASE_CACHE_PROBE" prepare-with-blobs "$LAZY_OCI" "${LAZY_PLATFORM#linux/}" "$CASE_ROOT/empty-blobs" > incomplete.stdout 2> incomplete.stderr
> grep -F 'read seeded OCI blob' incomplete.stderr
> test ! -s incomplete.stdout
> test -z "$(find "$CASE_ROOT/incomplete" -name HEAD.json -print)"
> cp -R "$seeds" "$CASE_ROOT/corrupt-blobs"
> python3 - "$CASE_ROOT/corrupt-blobs" <<'PY'
> import sys
> from pathlib import Path
> for blob in Path(sys.argv[1]).iterdir():
>     data = blob.read_bytes()
>     blob.write_bytes(bytes([data[0] ^ 1]) + data[1:])
> PY
> expect_refused env PVISOR_CACHE_LOCATION="$CASE_ROOT/incomplete" "$PVISOR_CASE_CACHE_PROBE" prepare-with-blobs "$LAZY_OCI" "${LAZY_PLATFORM#linux/}" "$CASE_ROOT/corrupt-blobs" > incomplete.stdout 2> incomplete.stderr
> grep -F 'is corrupt' incomplete.stderr
> test ! -s incomplete.stdout
> test -z "$(find "$CASE_ROOT/incomplete" -name HEAD.json -print)"
> test -z "$(find "$PVISOR_IMAGE_STORE/staging" -mindepth 1 -print)"
> lazy_stop_registry
> rm -rf -- "$PVISOR_IMAGE_STORE"
> snapshot shared "$shared"
> export PVISOR_CACHE_READ_ONLY=true
> lazy_cache_prepare "$LAZY_OCI" > reopened.json
> json_expect reopened.json /image_handle "\"$handle\""
> "$PVISOR_CASE_CACHE_PROBE" read-file "$handle" small-099 > small
> lazy_start_repository "$shared"
> env -u PVISOR_CACHE_BACKEND -u PVISOR_CACHE_READ_ONLY PVISOR_CACHE_LOCATION="$LAZY_REPOSITORY" "$PVISOR_CASE_CACHE_PROBE" read-file "$handle" large > large
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
> python3 - "$shared" "$CASE_ROOT/requests.jsonl" <<'PY'
> import hashlib, json, sys
> from pathlib import Path
> requests = [json.loads(line) for line in Path(sys.argv[2]).read_text().splitlines()]
> blobs = {request['path'] for request in requests if request['path'].startswith('/data/blobs/')}
> assert len(blobs) == 1
> path = Path(sys.argv[1]) / blobs.pop().lstrip('/')
> assert hashlib.sha256(path.read_bytes()).hexdigest() == path.name
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
