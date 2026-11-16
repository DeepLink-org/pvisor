# S-USE-005：查看 Agent 看到的文件

准备让 Agent 修改一份报告，先检查暂存内容与原件的区别。`inspect` 提供只读视图。

**语义**：原件保持 original，inspect 读到 proposed，review 列出报告改动。

**违反示例**：写入穿透原件或 review 漏报改动。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-USE-005 --timeout 180 <<'CASE'
> journey_setup
> printf original > report.txt
> pvisor spawn --stage "$CASE_ROOT/draft" -- /bin/sh -c 'printf proposed > report.txt'
> assert_content report.txt original
> pvisor inspect "$CASE_ROOT/draft" -- /bin/cat report.txt > "$CASE_ROOT/view.txt"
> assert_content "$CASE_ROOT/view.txt" proposed
> pvisor review "$CASE_ROOT/draft" --diff > "$CASE_ROOT/review.txt"
> journey_contains "$CASE_ROOT/review.txt" report.txt
> pvisor review "$CASE_ROOT/draft" --json > "$CASE_ROOT/review.json"
> json_paths "$CASE_ROOT/review.json" report.txt
> CASE
PASS S-USE-005
```
