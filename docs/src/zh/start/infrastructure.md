<a id="vm"></a>

# 执行基础设施：工具执行接入示例 {#first-task}

通过 pVisor CLI 在 VM 中运行工具命令，将结果交回 Agent、评测器或训练流程。

示例选取 `qutebrowser-667` SWE 轨迹中的候选补丁和两条工具命令：运行针对性测试，再检查修改后模块的语法。两条命令共享同一个 VM 和工作区，执行结束后统一返回结果，无需配置模型 API。

## 1. 准备环境 {#prerequisites}

完成[执行基础设施环境准备](installation.md#vm-and-oci)，在该节点的仓库根目录运行示例；尚未获取仓库时：

```bash
git clone https://github.com/DeepLink-org/pvisor.git
cd pvisor
```

仓库包含调用脚本和任务材料，任务依赖由镜像提供。

以下命令使用 Bash。在已激活 pVisor 安装环境的同一个终端确认工具：

```bash
pvisor --version
python3 --version
git --version
bash --version
test -r /dev/kvm && test -w /dev/kvm && echo 'KVM accessible'
```

应看到版本信息和 `KVM accessible`。缺少任一条件时，先回到[安装指南](installation.md#vm-and-oci)修复。示例的 Python 调用脚本在宿主执行，任务测试在 VM 内执行，两套环境分开。

## 2. 运行示例 {#start-task}

```bash
bash examples/pvisor/07-vm-task/run.sh
```

首次运行会准备并缓存固定任务镜像，耗时取决于镜像下载与解包；后续运行可复用镜像缓存。无需手工启动 VM。示例使用 1 vCPU 和 1 GiB guest RAM，关闭 guest 网络；每条工具命令限时 180 秒，guest 脚本限时 300 秒，镜像准备不计入该限时。

成功时，终端显示：

```text
Step 42: exit_code=0, timed_out=False
...
7 passed
Step 43: exit_code=0, timed_out=False
Syntax OK
```

末尾还应看到 `Job: …; Attempt: …` 和 `Results: …/sample.json`。路径每次运行不同，请保留本次 `Results` 路径。

## 3. 打开并核对反馈 {#read-feedback}

在同一个终端读取记录。运行下面命令，在提示后粘贴 `Results:` 后面的完整路径（不包含 `Results:` 本身）：

```bash
read -r -p 'Paste Results path: ' results_path
python3 -m json.tool "$results_path"
```

JSON 应包含任务和执行身份，以及两条工具调用的 `steps`。逐项核对 `exit_code` 为 0、`timed_out` 为 false，并查看 `stdout` 中的 `7 passed` 与 `Syntax OK`。成功调用的反馈应与终端摘要一致。

同一目录中的 `bundle.json` 保存 pVisor 执行记录，调用方从 `sample.json` 读取工具反馈。选中的测试与语法检查通过，不代表完整 SWE 评测或原任务全部要求通过。

## 4. 确认清理与失败状态 {#cleanup}

全部工具成功时，适配器收集并保存反馈后释放 stage，记录目录仍保留。不需要再对 `last` 执行 apply/drop；此示例自行管理它的任务与 stage，也不会把候选补丁自动合入宿主项目。

工具返回非零或超时时，反馈和 stage 会保留，调用脚本返回失败。VM、准备或收集错误会显示 `Execution error`；保留报错及已有记录，按[示例错误契约](https://github.com/DeepLink-org/pvisor/tree/main/examples/pvisor/07-vm-task)定位后再清理，避免用 `last` 操作其他任务。

| 现象 | 下一步 |
| --- | --- |
| 镜像下载失败 | 检查 Docker Hub 的解析、访问和缓存磁盘空间；离线环境使用[相同任务镜像的 rootfs](../guides/executors/vm.md#rootfs)。 |
| KVM 或 namespace 权限错误 | 回到[VM 前置条件](installation.md#vm-and-oci)修复。 |
| 工具超时或测试失败 | 检查 `sample.json` 中该步的输出、退出码与超时状态；区分工具失败和执行基础设施错误。 |
| 清理失败 | 反馈已先保存，保留记录和错误；按示例 README 检查 stage 状态。 |

示例 README 记录了一次使用离线 rootfs 的 Linux/KVM 运行。首次在线拉取和其他环境仍需验证。

<a id="framework-integration"></a>

## 5. 在你的调用流程中使用 {#collect-results}

在集群中，框架分发任务，Worker 调用 pVisor 执行工具，再将反馈交回框架。这个示例提供可放入 Worker 的调用逻辑与执行适配器。

`run.py` 准备任务并处理结果，`executor.py` 通过 CLI 执行任务。调用方用下面的调用提交任务：

```python
feedback = execute(task, EXAMPLE / "candidate.patch")
```

你可以沿用这个结构，替换任务镜像、候选补丁和工具命令，再将返回的反馈交给 Agent、评测器或训练数据收集流程。

示例中的 `execute` 是一个同步执行固定命令批次的适配函数，不包含远程调度或根据反馈生成下一次调用的 Agent 循环。

任务格式、错误处理和清理规则见[示例 README](https://github.com/DeepLink-org/pvisor/tree/main/examples/pvisor/07-vm-task)。

<a id="pause-resume"></a>
<a id="offload-resume"></a>

下一步：[暂停、卸载与恢复 VM](../guides/infrastructure/lifecycle.md) · [Runtime 与 Daemon 接入方式](../guides/infrastructure/integration.md)
