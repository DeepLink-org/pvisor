# S-USE-006：只接受需要的文件

Agent 同时修改正文和实验文件时，可以只接受正文，再丢弃剩余改动。

**语义**：apply --path 只提交 report.txt，剩余 scratch.txt 被 drop 丢弃，Session 记录仍可查询。

**违反示例**：选择性 apply 提交所有文件，或 drop 删除记录。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-USE-006 --timeout 180 <<'CASE'
> journey_setup
> pvisor spawn --stage "$CASE_ROOT/draft" -- /bin/sh -c 'printf accepted > report.txt; printf scratch > scratch.txt'
> pvisor apply "$CASE_ROOT/draft" --path report.txt
> assert_content report.txt accepted
> assert_absent scratch.txt
> pvisor review "$CASE_ROOT/draft" --json > "$CASE_ROOT/review.json"
> json_paths "$CASE_ROOT/review.json" scratch.txt
> pvisor drop "$CASE_ROOT/draft"
> assert_absent scratch.txt
> pvisor review "$CASE_ROOT/draft" --json > "$CASE_ROOT/review.json"
> json_paths "$CASE_ROOT/review.json"
> CASE
PASS S-USE-006
```
