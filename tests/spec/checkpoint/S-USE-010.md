# S-USE-010：接受文件后仍能审查保存点

对当前提案做决定之前保存 checkpoint，接受后仍可以审查原提案。

**语义**：apply 后当前改动为空；checkpoint review 仍列出 report.txt，已全部 apply 的 Session 拒绝 drop，checkpoint 保持。

**违反示例**：apply/drop 改写不可变保存点。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-USE-010 --timeout 180 <<'CASE'
> journey_setup
> pvisor spawn --stage "$CASE_ROOT/source" -- /bin/sh -c 'printf proposal > report.txt'
> pvisor checkpoint create "$CASE_ROOT/source" --json > "$CASE_ROOT/checkpoint.json"
> checkpoint=$(journey_json "$CASE_ROOT/checkpoint.json" /checkpoint_id)
> pvisor apply "$CASE_ROOT/source" --all
> assert_content report.txt proposal
> pvisor review "$CASE_ROOT/source" --json > "$CASE_ROOT/current.json"
> json_paths "$CASE_ROOT/current.json"
> pvisor review "$CASE_ROOT/source" --checkpoint "$checkpoint" --json > "$CASE_ROOT/saved.json"
> json_paths "$CASE_ROOT/saved.json" report.txt
> expect_refused pvisor drop "$CASE_ROOT/source"
> assert_content report.txt proposal
> pvisor checkpoint show "$CASE_ROOT/source" "$checkpoint" --json > "$CASE_ROOT/show.json"
> CASE
PASS S-USE-010
```
