# S-USE-009：重复请求只保存一次

自动化调用方可能重试请求。固定 `--request-id`，再用 list/show 查同一个保存点。

**语义**：相同请求返回同一 checkpoint，第二次 reused=true，list 只有一条。

**违反示例**：重试产生重复 checkpoint 或不同 ID。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-USE-009 --timeout 180 <<'CASE'
> journey_setup
> pvisor spawn --stage "$CASE_ROOT/source" -- /bin/sh -c 'printf proposal > report.txt'
> pvisor checkpoint create "$CASE_ROOT/source" --request-id before-refactor --json > "$CASE_ROOT/first.json"
> pvisor checkpoint create "$CASE_ROOT/source" --request-id before-refactor --json > "$CASE_ROOT/retry.json"
> checkpoint=$(journey_json "$CASE_ROOT/first.json" /checkpoint_id)
> json_expect "$CASE_ROOT/retry.json" /checkpoint_id "\"$checkpoint\""
> json_expect "$CASE_ROOT/retry.json" /reused true
> pvisor checkpoint list "$CASE_ROOT/source" --json > "$CASE_ROOT/list.json"
> json_expect "$CASE_ROOT/list.json" /checkpoints/0/checkpoint_id "\"$checkpoint\""
> json_length "$CASE_ROOT/list.json" /checkpoints 1
> pvisor checkpoint show "$CASE_ROOT/source" "$checkpoint" --json > "$CASE_ROOT/show.json"
> json_expect "$CASE_ROOT/show.json" /branch_references 0
> CASE
PASS S-USE-009
```
