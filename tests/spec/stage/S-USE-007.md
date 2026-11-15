# S-USE-007：宿主已修改时拒绝覆盖

审查期间你也修改了同一文件。apply 必须保护新的宿主内容，并保留 Agent 提案供处理。

**语义**：有 preimage 冲突的 apply 失败，宿主内容为 human，暂存内容仍为 agent。

**违反示例**：用旧提案覆盖人类的新改动，或失败时丢掉提案。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-USE-007 --timeout 180 <<'CASE'
> journey_setup
> printf base > report.txt
> pvisor spawn --stage "$CASE_ROOT/draft" -- /bin/sh -c 'printf agent > report.txt'
> printf human > report.txt
> expect_refused pvisor apply "$CASE_ROOT/draft" --all
> assert_content report.txt human
> pvisor inspect "$CASE_ROOT/draft" -- /bin/cat report.txt > "$CASE_ROOT/view.txt"
> assert_content "$CASE_ROOT/view.txt" agent
> CASE
PASS S-USE-007
```
