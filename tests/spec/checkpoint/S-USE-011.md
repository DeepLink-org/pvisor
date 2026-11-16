# S-USE-011：从同一提案启动两个分支

比较两种实现方案时，从相同 checkpoint 启动两次 fork。源提案和每个子 Session 都各自保留。

**语义**：两个分支都读到 seed，分别写成 branch-a/branch-b，源仍为 seed，宿主仍无 report.txt；删除被引用 checkpoint 明确失败。

**违反示例**：分支互相污染、修改源提案或删除仍被引用的保存点。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-USE-011 --timeout 180 <<'CASE'
> journey_setup
> pvisor spawn --stage "$CASE_ROOT/source" -- /bin/sh -c 'printf seed > report.txt'
> pvisor checkpoint create "$CASE_ROOT/source" --json > "$CASE_ROOT/checkpoint.json"
> checkpoint=$(journey_json "$CASE_ROOT/checkpoint.json" /checkpoint_id)
> pvisor fork "$CASE_ROOT/source" --checkpoint "$checkpoint" --stage "$CASE_ROOT/branch-a" -- /bin/sh -c 'test "$(cat report.txt)" = seed; printf branch-a > report.txt'
> pvisor fork "$CASE_ROOT/source" --checkpoint "$checkpoint" --stage "$CASE_ROOT/branch-b" -- /bin/sh -c 'test "$(cat report.txt)" = seed; printf branch-b > report.txt'
> for branch in source branch-a branch-b; do
>   pvisor inspect "$CASE_ROOT/$branch" -- /bin/cat report.txt > "$CASE_ROOT/$branch.txt"
> done
> assert_content "$CASE_ROOT/source.txt" seed
> assert_content "$CASE_ROOT/branch-a.txt" branch-a
> assert_content "$CASE_ROOT/branch-b.txt" branch-b
> assert_absent report.txt
> pvisor checkpoint show "$CASE_ROOT/source" "$checkpoint" --json > "$CASE_ROOT/show.json"
> json_expect "$CASE_ROOT/show.json" /branch_references 2
> expect_refused pvisor checkpoint delete "$CASE_ROOT/source" "$checkpoint"
> CASE
PASS S-USE-011
```
