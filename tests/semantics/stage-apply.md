# Stage / Apply / Drop 语义（未审核草稿）

这些规格表达设计承诺；测试通过不等于经过人工批准。

### S-STAGE-001：暂存 Job 不改变工作区

<!-- semantic-case: requires=stage -->

**语义**：Job 成功、非零退出或被信号终止后，apply 前的路径、类型、完整内容、权限位和链接目标均与开始前一致。

**违反示例**：删除穿透 lower，或异常清理把 upper 自动合并到工作区。

```bash
printf original > edit; mkdir d; printf keep > d/keep
snapshot before "$WS"
expect_exit 0 stage "$CASE_ROOT/success" 'printf changed > edit; rm -rf d; printf new > added'
assert_unchanged before "$WS"
expect_exit 3 stage "$CASE_ROOT/nonzero" 'printf changed > edit; rm -rf d; exit 3'
assert_unchanged before "$WS"
expect_refused stage "$CASE_ROOT/signal" 'printf changed > edit; rm -rf d; kill -TERM $$'
assert_unchanged before "$WS"
```

### S-STAGE-002：Job 读取自己的变更

<!-- semantic-case: requires=stage -->

**语义**：Job 内读取看到自己的写入、删除和重命名；apply 前宿主工作区不变。

**违反示例**：写完读取旧内容，删除后仍可见，或重命名后找不到新路径。

```bash
printf original > edit; printf gone > deleted; printf moved > old
snapshot before "$WS"
stage "$CASE_ROOT/stage" 'printf changed > edit; test "$(cat edit)" = changed; rm deleted; test ! -e deleted; mv old new; test ! -e old; test "$(cat new)" = moved'
assert_unchanged before "$WS"
```

### S-STAGE-003：Review 精确表示净效果

<!-- semantic-case: requires=stage -->

**语义**：review 清单精确等于文件净增删改；创建又删除的临时文件不出现在清单中。

**违反示例**：临时文件泄漏进 review，遗漏删除，或只记录被触碰而无净效果的路径。

```bash
printf original > edit; printf gone > deleted
stage "$CASE_ROOT/stage" 'printf changed > edit; rm deleted; printf added > added; printf temporary > temporary; rm temporary'
assert_changes "$CASE_ROOT/stage" <<'EXPECTED'
added added
deleted deleted
modified edit
EXPECTED
```

### S-STAGE-004：Apply all 等价于直接运行

<!-- semantic-case: requires=stage -->

**语义**：对同一初始工作区执行同一成功脚本，stage 后 apply --all 与直接执行所得完整树相同。

**违反示例**：内容相同但权限或链接丢失，或者 rename、目录删除结果不同。

```bash
printf original > edit; mkdir d; printf gone > d/file; printf moved > old
mkdir "$CASE_ROOT/direct"; cp -pR "$WS/." "$CASE_ROOT/direct/"
script='printf changed > edit; rm -rf d; mv old renamed; mkdir newdir; printf new > newdir/file; chmod 700 renamed'
(cd "$CASE_ROOT/direct"; /bin/sh -eu -c "$script")
stage "$CASE_ROOT/stage" "$script"
pvisor apply --all "$CASE_ROOT/stage"
assert_same_tree "$WS" "$CASE_ROOT/direct"
```

### S-STAGE-005：Drop 保留工作区并拒绝后续 apply

<!-- semantic-case: requires=stage -->

**语义**：drop 后工作区不变；该 stage 后续 apply 必须拒绝且无效果。

**违反示例**：drop 把变更落地，或丢弃后还能 apply。

```bash
printf original > edit
snapshot before "$WS"
stage "$CASE_ROOT/stage" 'printf changed > edit; printf new > added'
pvisor drop "$CASE_ROOT/stage"
assert_unchanged before "$WS"
expect_refused pvisor apply --all "$CASE_ROOT/stage"
assert_unchanged before "$WS"
```

### S-STAGE-006：选择性 Apply 限于所选子树

<!-- semantic-case: requires=stage -->

**语义**：apply --path P 只改变 P 及其后代；覆盖全部变更的两次选择性 apply 等价于一次 --all。

**违反示例**：选择目录时修改兄弟文件，或第一次 apply 消耗未选变更。

```bash
mkdir chosen other; printf a > chosen/a; printf b > other/b
mkdir "$CASE_ROOT/all"; cp -pR "$WS/." "$CASE_ROOT/all/"
script='printf changed > chosen/a; printf new > chosen/new; printf changed > other/b'
stage "$CASE_ROOT/stage" "$script"
(cd "$CASE_ROOT/all"; stage "$CASE_ROOT/all-stage" "$script")
snapshot other "$WS/other"
pvisor apply --path chosen "$CASE_ROOT/stage"
assert_content chosen/a changed; assert_content chosen/new new
assert_unchanged other "$WS/other"
pvisor apply --path other "$CASE_ROOT/stage"
pvisor apply --all "$CASE_ROOT/all-stage"
assert_same_tree "$WS" "$CASE_ROOT/all"
```

### S-STAGE-007：重复 Apply 无额外效果

<!-- semantic-case: requires=stage -->

**语义**：成功 apply 的同一路径再次 apply 不改变工作区；是否报告 already applied 不影响该性质。

**违反示例**：重复删除或重复复制引入新修改。

```bash
printf original > edit
stage "$CASE_ROOT/stage" 'printf changed > edit'
pvisor apply --path edit "$CASE_ROOT/stage"
snapshot applied "$WS"
# 重复请求可以成功或明确拒绝，但必须无效果。
pvisor apply --path edit "$CASE_ROOT/stage" || :
assert_unchanged applied "$WS"
```

### S-STAGE-008：外部变更触发冲突

<!-- semantic-case: requires=stage -->

**语义**：staging 后目标被外部修改、新建或删除时，apply 被拒绝，外部状态完整保留。

**违反示例**：覆盖外部修改，覆盖外部新建文件，或恢复外部删除文件。

```bash
printf original > modified; printf original > removed
stage "$CASE_ROOT/stage" 'printf agent > modified; printf agent > created; printf agent > removed'
printf external > modified; printf external > created; rm removed
snapshot external "$WS"
for path in modified created removed; do
  expect_refused pvisor apply --path "$path" "$CASE_ROOT/stage"
  assert_unchanged external "$WS"
done
```

### S-STAGE-009：冲突导致所选集合全部无效果

<!-- semantic-case: requires=stage -->

**语义**：所选集合任一路径冲突时，该次 apply 不改变所选集合中任何路径。

**违反示例**：先应用无冲突路径，后发现冲突退出，留下部分落地。

```bash
printf original > a; printf original > z
stage "$CASE_ROOT/stage" 'printf agent > a; printf agent > z'
printf external > z
snapshot external "$WS"
expect_refused pvisor apply --all "$CASE_ROOT/stage"
assert_unchanged external "$WS"
```

### S-STAGE-010：未触碰路径的外部变更不阻止 Apply

<!-- semantic-case: requires=stage -->

**语义**：Agent 未触碰路径的外部修改、新增和删除不影响 apply，并全部保留。

**违反示例**：整树冲突检查误拒绝，或 apply 恢复无关外部删除。

```bash
printf original > touched; mkdir other; printf base > other/edit; printf base > other/deleted
stage "$CASE_ROOT/stage" 'printf agent > touched'
printf external > other/edit; printf external > other/new; rm other/deleted
snapshot external "$WS/other"
pvisor apply --all "$CASE_ROOT/stage"
assert_content touched agent
assert_unchanged external "$WS/other"
```

### S-STAGE-011：删除目录不吞掉外部新增

<!-- semantic-case: requires=stage -->

**语义**：Agent 删除目录后，外部向目录新增文件，apply 必须拒绝并保留整个外部树。

**违反示例**：仅核对目录旧文件，递归删除新文件。

```bash
mkdir d; printf base > d/base
stage "$CASE_ROOT/stage" 'rm -rf d'
printf external > d/new
snapshot external "$WS"
expect_refused pvisor apply --all "$CASE_ROOT/stage"
assert_unchanged external "$WS"
```

### S-STAGE-012：Apply 不越出工作区

<!-- semantic-case: requires=stage -->

**语义**：staging 后目录被替换为指向外部的链接时，apply 不写外部目录；本例要求冲突拒绝且两棵树不变。

**违反示例**：沿替换的链接把 staged 内容写入工作区之外。

```bash
mkdir d; printf base > d/file
stage "$CASE_ROOT/stage" 'printf agent > d/file'
mkdir "$CASE_ROOT/outside"; printf external > "$CASE_ROOT/outside/file"
rm -rf d; ln -s "$CASE_ROOT/outside" d
snapshot workspace "$WS"; snapshot outside "$CASE_ROOT/outside"
expect_refused pvisor apply --all "$CASE_ROOT/stage"
assert_unchanged workspace "$WS"; assert_unchanged outside "$CASE_ROOT/outside"
```

### S-STAGE-013：调用返回值与链接效果一致

<!-- semantic-case: requires=stage xfail-on=macos xfail-reason="macFUSE 创建链接返回 EPERM 但已有实际效果；docs/semspec-design.md §12" -->

**语义**：创建链接的调用成功必须产生对应链接，失败则不得产生链接；返回值和可观察效果一致。

**违反示例**：ln 返回 EPERM，却已创建可 apply 的链接。

```bash
stage "$CASE_ROOT/stage" 'code=0; ln -s /etc/hosts link || code=$?; if test "$code" = 0; then test -L link; test "$(readlink link)" = /etc/hosts; else test ! -e link; test ! -L link; fi'
assert_absent link
```

### S-STAGE-014：Apply 保留可执行位和链接目标

<!-- semantic-case: requires=stage -->

**语义**：可执行位和符号链接目标原样落地，链接不得被解引用为目标文件。

**违反示例**：可执行文件失去 x 位，或链接落地为 /etc/hosts 内容副本。

```bash
stage "$CASE_ROOT/stage" 'printf executable > executable; chmod 755 executable; ln -s executable link'
pvisor apply --all "$CASE_ROOT/stage"
assert_content executable executable
[ -x executable ] || fail 'executable bit lost'
[ -L link ] && [ "$(readlink link)" = executable ] || fail 'link was dereferenced or changed'
```
