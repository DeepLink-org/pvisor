<a id="_1"></a>

# 本地工作流：运行与审查 {#local-workflow}

用一个演示脚本修改文件，再审查并选择性合入，体验完整的本地工作流。无需 Agent 账号或 API Key；请先完成[安装](installation.md)，包括宿主机暂存所需的 FUSE/macFUSE。

## 1. 准备演示项目

```bash
mkdir -p pvisor-demo/project/src
cd pvisor-demo/project
printf 'print("v1")\n' > src/app.py
printf 'legacy\n' > src/legacy.txt
git init -q
git add src
git -c user.name='pVisor Demo' -c user.email='demo@example.invalid' -c commit.gpgsign=false commit -qm init
```

演示目录应是新目录。Git 身份仅用于这次基线提交，不修改你的全局配置。

<a id="2-agent"></a>

## 2. 编写演示脚本 {#demo-script}

脚本会修改 `src/app.py`、删除 `src/legacy.txt`，并创建不准备合入的 `scratch.txt`：

```bash
cat > ../agent.sh <<'SH'
#!/usr/bin/env bash
set -eu
printf 'print("patched")\n' >> src/app.py
rm src/legacy.txt
printf 'scratch\n' > scratch.txt
SH
chmod +x ../agent.sh
```

<a id="3"></a>

## 3. 运行并暂存改动 {#run-and-stage}

```bash
pvisor spawn --safe --overlaynet-deny-all --stage ../stage-001 -- ../agent.sh
```

`--safe` 要求文件与网络隔离，无法落实时拒绝启动；`--overlaynet-deny-all` 强制阻断外部网络访问。`--stage` 指定候选改动的存储位置，应放在项目外，每次运行使用新目录。

**预期结果：**脚本正常退出，三项改动留在暂存目录，原项目尚未变化。在当前终端检查：

```bash
cat src/app.py
cat src/legacy.txt
test ! -e scratch.txt && echo 'scratch.txt: absent'
```

应输出：

```text
print("v1")
legacy
scratch.txt: absent
```

<a id="4"></a>

## 4. 审查候选改动 {#review-changes}

用暂存路径定位这次 Job：

```bash
pvisor status --review ../stage-001
```

**预期结果：**记录中包含 `src/app.py` 的修改、`src/legacy.txt` 的删除和 `scratch.txt` 的新增。核对退出码、实际生效的控制与警告，再决定接受哪些改动；运行结束不代表改动已合入。

需要进一步验证访问限制时，按[文件策略](../guides/policies/files.md)和[网络控制](../guides/policies/network.md)配置。普通 host 选择性代理只记录经过代理的请求，记录中没有某个目标不等于它从未被访问；此演示使用 deny-all 强制边界。

## 5. 只合入你要的部分

```bash
pvisor apply ../stage-001 --path src
pvisor drop ../stage-001
git diff -- src
```

**预期结果：**

| 文件 | 合入前 | 合入并丢弃剩余改动后 |
| --- | --- | --- |
| `src/app.py` | 只有 `print("v1")` | 新增一行 `print("patched")` |
| `src/legacy.txt` | 仍然存在 | 已删除 |
| `scratch.txt` | 项目中不存在，仅在 stage 中 | 项目中仍不存在，候选已丢弃 |

`apply --path src` 只接受 `src` 下的改动；`drop` 丢弃剩余候选，不会撤销已经接受的修改。`git diff -- src` 应显示源码新增行和文件删除。若你在运行期间修改了同一文件，`apply` 会拒绝覆盖；处理方式见[审查并应用改动](../guides/review-apply.md)。

<a id="6"></a>
<a id="7"></a>

## 下一步 {#next-steps}

接入真实 Agent 时，先确认它已安装在所选环境中，再按 [Agent 接入](../guides/agents/index.md)配置模型服务与凭据，将演示命令替换为 Agent CLI。使用自定义 `--stage` 时，后续操作继续传入该路径或输出的 Job ID；`last` 只查询默认存储。

需要 Linux 镜像或独立内核时，先[选择执行环境](../guides/executors/index.md)；需要从已有文件状态重试或并行探索时，使用[检查点与分叉](../guides/fork-checkpoint.md)及[并行工作区流程](../guides/parallel-agents.md#batch)。
