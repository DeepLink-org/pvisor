# semspec 自身的审核承诺（未审核草稿）

夹具台账均在 CASE_ROOT 内，与项目真实批准记录无关。
S-REVIEW-004 只能由人在交互环境外验证；AI 不得运行包含它的 run。

### S-REVIEW-001：陈述变化使已有审核失效

<!-- semantic-case: requires=python -->

**语义**：审核后改变 case 的非规范化文本，状态从 REVIEWED 变为 STALE；相同脚本成功不恢复审核。

**违反示例**：只给检查块计算摘要，改写语义文字仍然显示 REVIEWED。

```bash
fixture
fixture_state | grep -q REVIEWED
python3 - "$CASE_ROOT/project/spec/case.md" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1]); p.write_text(p.read_text().replace('preserve the result', 'alter the result'))
PY
fixture_state | grep -q STALE
```

### S-REVIEW-002：词汇变化使依赖审核失效

<!-- semantic-case: requires=python -->

**语义**：词汇文件的规范化字节改变后，依赖它的 case 必须成为 STALE。

**违反示例**：断言词汇变成空实现，但 case 审核状态不变。

```bash
fixture
fixture_state | grep -q REVIEWED
printf '# changed vocabulary\n' >> "$CASE_ROOT/project/spec/vocab/fixture.sh"
fixture_state | grep -q STALE
```

### S-REVIEW-003：XPASS 是失败

<!-- semantic-case: requires=python -->

**语义**：标注为预期失败的 case 实际成功时报告 XPASS，并使 run 以 1 退出，不能当作成功。

**违反示例**：通过的 xfail 静默变为 PASS，旧例外永远不需重审。

```bash
fixture
python3 - "$CASE_ROOT/project/spec/case.md" <<'PY'
import pathlib, sys
p = pathlib.Path(sys.argv[1]); p.write_text(p.read_text().replace('**语义**', '<!-- semantic-case: xfail-on=all xfail-reason="fixture" -->\n\n**语义**'))
PY
expect_exit 1 "$SUBJECT_BIN" --config "$CASE_ROOT/project/semspec.toml" run --format json --output "$CASE_ROOT/report.json"
[ "$("$SEMSPEC_BIN" helper json-get "$CASE_ROOT/report.json" /results/0/verdict/verdict)" = '"XPASS"' ] || fail 'XPASS missing'
```

### S-REVIEW-004：非交互批准被拒绝

<!-- semantic-case: requires=python -->

**语义**：stdin 非 TTY 时 approve 以 2 拒绝，夹具台账逐字节不变。

**违反示例**：流水线或自动化脚本无需人确认即可创建审批记录。

```bash
fixture
cp "$CASE_ROOT/project/REVIEWED.toml" "$CASE_ROOT/before.toml"
expect_exit 2 "$SUBJECT_BIN" --config "$CASE_ROOT/project/semspec.toml" approve S-FIXTURE-001 --reviewer simulated < /dev/null
cmp "$CASE_ROOT/before.toml" "$CASE_ROOT/project/REVIEWED.toml" || fail 'ledger changed without human review'
```
