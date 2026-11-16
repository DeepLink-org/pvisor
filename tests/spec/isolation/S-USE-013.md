# S-USE-013：拒绝敏感路径访问

工作区里也可能有凭据。显式 `--access` 规则保护匹配路径，并检查任务确实遇到拒绝。

**语义**：Agent 不能读取 secret.txt，却仍可生成 public.txt，宿主 secret 保持。

**违反示例**：拒绝规则只记录日志而仍返回秘密。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-USE-013 --timeout 180 <<'CASE'
> journey_setup
> printf sentinel > secret.txt
> pvisor spawn --safe --no-agent-defaults --overlaynet-deny-all --stage "$CASE_ROOT/policy" --access 'secret.txt:deny' -- /bin/sh -c 'if cat secret.txt > leaked.txt; then exit 9; fi; printf public > public.txt'
> assert_content secret.txt sentinel
> assert_absent public.txt
> pvisor inspect "$CASE_ROOT/policy" -- /bin/cat public.txt > "$CASE_ROOT/public.txt"
> assert_content "$CASE_ROOT/public.txt" public
> CASE
PASS S-USE-013
```
