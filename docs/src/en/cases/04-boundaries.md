# 4. Set boundaries for untrusted work

Move from file staging to execution isolation. --safe requires an available isolation implementation and retains workspace edits. Declare network and path policies, then inspect the actual evidence in the Run Bundle. Tests use a private temporary HOME and XDG directories to avoid personal configuration; check production filesystem capabilities separately.

### S-USE-012: Protect files outside the workspace

An untrusted script may write absolute paths. Use safe and verify outside files are unchanged rather than assuming staging proves isolation.

**Contract**: Workspace writes are staged, the original outside file is invisible and retains sentinel on the host; private /tmp writes do not reach the host, and evidence reports filesystem_non_bypassable.

**Violation**: An outside file is overwritten or evidence claims a boundary that was not enforced.

```bash
journey_setup
printf sentinel > "$CASE_ROOT/outside.txt"
pvisor run --safe --no-agent-defaults --overlaynet-deny-all --stage "$CASE_ROOT/safe" -- /bin/sh -c 'printf staged > report.txt; test ! -e "$1"; printf private-temp > "$1"' sh "$CASE_ROOT/outside.txt"
assert_content "$CASE_ROOT/outside.txt" sentinel
assert_absent report.txt
pvisor review "$CASE_ROOT/safe" --json > "$CASE_ROOT/review.json"
json_expect "$CASE_ROOT/review.json" /safety/filesystem_non_bypassable true
json_paths "$CASE_ROOT/review.json" report.txt
```

### S-USE-013: Deny access to sensitive paths

Credentials may be inside the workspace. Protect matching paths with --access and verify the task receives a refusal.

**Contract**: The Agent cannot read secret.txt but can write public.txt; the host secret remains unchanged.

**Violation**: The deny rule logs an event but returns the secret.

```bash
journey_setup
printf sentinel > secret.txt
pvisor run --safe --no-agent-defaults --overlaynet-deny-all --stage "$CASE_ROOT/policy" --access 'secret.txt:deny' -- /bin/sh -c 'if cat secret.txt > leaked.txt; then exit 9; fi; printf public > public.txt'
assert_content secret.txt sentinel
assert_absent public.txt
pvisor inspect "$CASE_ROOT/policy" -- /bin/cat public.txt > "$CASE_ROOT/public.txt"
assert_content "$CASE_ROOT/public.txt" public
```

### S-USE-014: Deny networking and test the refusal

Use deny-all for offline work that needs no API. Test against a local listener to avoid reliance on a public service.

**Contract**: The host can reach the listener; the deny-all Job cannot, and network evidence is non_bypassable.

**Violation**: Network isolation silently degrades and the Agent reaches the host listener.

```bash
journey_setup
journey_tools listen "$CASE_ROOT/port" &
server=$!
trap 'kill "$server" 2>/dev/null || true; wait "$server" 2>/dev/null || true' EXIT
journey_wait_file "$CASE_ROOT/port" "$server"
port=$(cat "$CASE_ROOT/port")
python3 -c 'import socket, sys; socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=1).close()' "$port"
pvisor run --overlaynet-deny-all --stdio capture -- /usr/bin/python3 -c '
import socket, sys
try:
    socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=1).close()
except OSError:
    print("connection-denied")
else:
    raise SystemExit("network boundary bypassed")
' "$port"
journey_bundle > "$CASE_ROOT/bundle.json"
json_expect "$CASE_ROOT/bundle.json" /run/output/stdout '"connection-denied\n"'
json_expect "$CASE_ROOT/bundle.json" /safety/network_non_bypassable true
kill "$server"
wait "$server" || true
trap - EXIT
```

[Learning path](index.md) · [5. Replay trajectories and choose restoration tools](05-tools-and-restoration.md)
