# S-USE-012：safe 保护工作区外的文件

不可信脚本可能尝试写入绝对路径。用 safe 并验证外部文件未变化，而不是仅看到 stage 就认为隔离。

**语义**：工作区改动暂存，原工作区外文件不可见且宿主保持 sentinel，私有 /tmp 写入不穿透宿主，Evidence 声明 filesystem_non_bypassable。

**违反示例**：外部文件被覆盖，或未执行隔离却报告边界有效。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" use S-USE-012 --timeout 180 <<'CASE'
> journey_setup
> printf sentinel > "$CASE_ROOT/outside.txt"
> pvisor spawn --safe --no-agent-defaults --overlaynet-deny-all --stage "$CASE_ROOT/safe" -- /bin/sh -c 'printf staged > report.txt; test ! -e "$1"; printf private-temp > "$1"' sh "$CASE_ROOT/outside.txt"
> assert_content "$CASE_ROOT/outside.txt" sentinel
> assert_absent report.txt
> pvisor review "$CASE_ROOT/safe" --json > "$CASE_ROOT/review.json"
> json_expect "$CASE_ROOT/review.json" /safety/filesystem_non_bypassable true
> json_paths "$CASE_ROOT/review.json" report.txt
> CASE
PASS S-USE-012
```
