<a id="_1"></a>

# 本地工作流：运行与审查 {#local-workflow}

使用本机工具和工作区运行演示脚本，再审查并选择性合入文件改动，无需 VM 镜像或模型 API。请先完成[本地工作流环境准备](installation.md#local-prerequisites)。

<a id="1"></a>

## 1. 准备演示项目与脚本 {#prepare-demo}

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

<a id="demo-script"></a>

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

## 2. 运行并暂存改动 {#run-and-stage}

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

## 3. 审查候选改动 {#review-changes}

用暂存路径定位这次 Job：

```bash
pvisor status --review ../stage-001
```

**预期结果：**记录中包含 `src/app.py` 的修改、`src/legacy.txt` 的删除和 `scratch.txt` 的新增。核对退出码、实际生效的控制与警告，再决定接受哪些改动；运行结束不代表改动已合入。

<a id="5"></a>

## 4. 只合入你要的部分 {#apply-changes}

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

将演示命令替换为自己的 Agent CLI，按 [Agent 接入](../guides/agents/index.md)准备所需工具、模型服务与凭据。需要 Linux 镜像或独立内核时，查阅[选择执行环境](../guides/executors/index.md)。
