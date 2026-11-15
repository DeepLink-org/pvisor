# Stage / Apply / Drop 语义（未审核草稿）

这些规格表达设计承诺；测试通过不等于经过人工批准。

### S-STAGE-001：暂存 Session 不改变工作区

**语义**：Session 成功、非零退出或被信号终止后，apply 前的路径、类型、完整内容、权限位和链接目标均与开始前一致。

**违反示例**：删除穿透 lower，或异常清理把 upper 自动合并到工作区。

运行与断言：[Scrut S-STAGE-001](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-001.md)。

### S-STAGE-002：Session 读取自己的变更

**语义**：Session 内读取看到自己的写入、删除和重命名；apply 前宿主工作区不变。

**违反示例**：写完读取旧内容，删除后仍可见，或重命名后找不到新路径。

运行与断言：[Scrut S-STAGE-002](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-002.md)。

### S-STAGE-003：Review 精确表示净效果

**语义**：review 清单精确等于文件净增删改；创建又删除的临时文件不出现在清单中。

**违反示例**：临时文件泄漏进 review，遗漏删除，或只记录被触碰而无净效果的路径。

运行与断言：[Scrut S-STAGE-003](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-003.md)。

### S-STAGE-004：Apply all 等价于直接运行

**语义**：对同一初始工作区执行同一成功脚本，stage 后 apply --all 与直接执行所得完整树相同。

**违反示例**：内容相同但权限或链接丢失，或者 rename、目录删除结果不同。

运行与断言：[Scrut S-STAGE-004](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-004.md)。

### S-STAGE-005：Drop 保留工作区并拒绝后续 apply

**语义**：drop 后工作区不变；该 stage 后续 apply 必须拒绝且无效果。

**违反示例**：drop 把变更落地，或丢弃后还能 apply。

运行与断言：[Scrut S-STAGE-005](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-005.md)。

### S-STAGE-006：选择性 Apply 限于所选子树

**语义**：apply --path P 只改变 P 及其后代；覆盖全部变更的两次选择性 apply 等价于一次 --all。

**违反示例**：选择目录时修改兄弟文件，或第一次 apply 消耗未选变更。

运行与断言：[Scrut S-STAGE-006](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-006.md)。

### S-STAGE-007：重复 Apply 无额外效果

**语义**：成功 apply 的同一路径再次 apply 不改变工作区；是否报告 already applied 不影响该性质。

**违反示例**：重复删除或重复复制引入新修改。

运行与断言：[Scrut S-STAGE-007](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-007.md)。

### S-STAGE-008：外部变更触发冲突

**语义**：staging 后目标被外部修改、新建或删除时，apply 被拒绝，外部状态完整保留。

**违反示例**：覆盖外部修改，覆盖外部新建文件，或恢复外部删除文件。

运行与断言：[Scrut S-STAGE-008](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-008.md)。

### S-STAGE-009：冲突导致所选集合全部无效果

**语义**：所选集合任一路径冲突时，该次 apply 不改变所选集合中任何路径。

**违反示例**：先应用无冲突路径，后发现冲突退出，留下部分落地。

运行与断言：[Scrut S-STAGE-009](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-009.md)。

### S-STAGE-010：未触碰路径的外部变更不阻止 Apply

**语义**：Agent 未触碰路径的外部修改、新增和删除不影响 apply，并全部保留。

**违反示例**：整树冲突检查误拒绝，或 apply 恢复无关外部删除。

运行与断言：[Scrut S-STAGE-010](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-010.md)。

### S-STAGE-011：删除目录不吞掉外部新增

**语义**：Agent 删除目录后，外部向目录新增文件，apply 必须拒绝并保留整个外部树。

**违反示例**：仅核对目录旧文件，递归删除新文件。

运行与断言：[Scrut S-STAGE-011](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-011.md)。

### S-STAGE-012：Apply 不越出工作区

**语义**：staging 后目录被替换为指向外部的链接时，apply 不写外部目录；本例要求冲突拒绝且两棵树不变。

**违反示例**：沿替换的链接把 staged 内容写入工作区之外。

运行与断言：[Scrut S-STAGE-012](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-012.md)。

### S-STAGE-013：调用返回值与链接效果一致


**语义**：创建链接的调用成功必须产生对应链接，失败则不得产生链接；返回值和可观察效果一致。

**违反示例**：ln 返回 EPERM，却已创建可 apply 的链接。

运行与断言：[Scrut S-STAGE-013](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-013.md)。

### S-STAGE-014：Apply 保留可执行位和链接目标

**语义**：可执行位和符号链接目标原样落地，链接不得被解引用为目标文件。

**违反示例**：可执行文件失去 x 位，或链接落地为 /etc/hosts 内容副本。

运行与断言：[Scrut S-STAGE-014](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-STAGE-014.md)。

## 准备与执行

先按第二章学习 stage、review、apply 与 drop，再运行这些独立检查验证净效果、冲突与链接行为。macOS 需要 macFUSE；Linux 需要 /dev/fuse 和 user/mount namespace。缺少前提时原检查退出 77，产品 fixture 报告 SKIP 并使门禁失败；S-STAGE-013 保留已知 macOS XFAIL，实际通过则以 XPASS 失败。检查和准备函数仍需人工审核。

```bash
mise run cases -- --suite stage
mise run cases -- --suite stage --case S-STAGE-008 --keep
```

准备与断言源码：[fixtures/stage.bash](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/fixtures/stage.bash)。
