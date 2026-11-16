# S-USE-014：禁止网络连接，验证真实拒绝

离线分析和不需 API 的任务可以 deny-all。用本机监听器验证连接确实失败，不依赖公网服务。

**语义**：宿主能连接测试监听器，deny-all Session 的相同连接失败且网络 Evidence 声明 non_bypassable。

**违反示例**：网络隔离静默退化，Agent 仍连到宿主监听器。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-USE-014 --timeout 180 <<'CASE'
> journey_setup
> journey_tools listen "$CASE_ROOT/port" &
> server=$!
> trap 'kill "$server" 2>/dev/null || true; wait "$server" 2>/dev/null || true' EXIT
> journey_wait_file "$CASE_ROOT/port" "$server"
> port=$(cat "$CASE_ROOT/port")
> python3 -c 'import socket, sys; socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=1).close()' "$port"
> pvisor spawn --overlaynet-deny-all --stdio capture -- /usr/bin/python3 -c '
> import socket, sys
> try:
>     socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=1).close()
> except OSError:
>     print("connection-denied")
> else:
>     raise SystemExit("network boundary bypassed")
> ' "$port"
> journey_bundle > "$CASE_ROOT/bundle.json"
> json_expect "$CASE_ROOT/bundle.json" /run/output/stdout '"connection-denied\n"'
> json_expect "$CASE_ROOT/bundle.json" /safety/network_non_bypassable true
> kill "$server"
> wait "$server" || true
> trap - EXIT
> CASE
PASS S-USE-014
```
