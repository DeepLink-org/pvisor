<a id="vm"></a>

# 执行基础设施：工具执行接入示例 {#first-task}

在 Agent 推理、评测或训练流程中，调用方需要把工具命令交给执行环境，再根据返回结果继续决策。这个示例展示如何通过 pVisor CLI，在 VM 中执行工具并收集反馈。

示例选取 `qutebrowser-667` SWE 轨迹中的候选补丁和两条工具命令：运行针对性测试，再检查修改后模块的语法。两条命令共享同一个 VM 和工作区，执行结束后统一返回结果，无需配置模型 API。

## 1. 准备环境 {#prerequisites}

完成[执行基础设施环境准备](installation.md#vm-and-oci)，在该节点的仓库根目录运行示例；尚未获取仓库时：

```bash
git clone https://github.com/DeepLink-org/pvisor.git
cd pvisor
```

仓库包含调用脚本和任务材料，任务依赖由镜像提供。

## 2. 运行示例 {#start-task}

```bash
bash examples/pvisor/07-vm-task/run.sh
```

成功时，终端显示：

```text
Step 42: exit_code=0, timed_out=False
...
7 passed
Step 43: exit_code=0, timed_out=False
Syntax OK
```

终端还会打印本次任务的记录目录，其中包含：

- `sample.json`：每条工具命令的输出、退出码、超时状态及关联的执行 ID。
- `bundle.json`：pVisor 执行记录。

本例展示如何执行一段 SWE 工具调用并收集反馈，运行内容为针对性测试和语法检查。

<a id="framework-integration"></a>

## 3. 在你的调用流程中使用 {#collect-results}

在集群中，框架分发任务，Worker 调用 pVisor 执行工具，再将反馈交回框架。这个示例提供可放入 Worker 的调用逻辑与执行适配器。

`run.py` 准备任务并处理结果，`executor.py` 通过 CLI 执行任务。两者通过以下调用连接：

```python
feedback = execute(task, EXAMPLE / "candidate.patch")
```

你可以沿用这个结构，替换任务镜像、候选补丁和工具命令，再将返回的反馈交给 Agent、评测器或训练数据收集流程。

示例中的 `execute` 是一个同步执行固定命令批次的适配函数，不包含远程调度或根据反馈生成下一次调用的 Agent 循环。

任务格式、错误处理和清理规则见[示例 README](https://github.com/DeepLink-org/pvisor/tree/main/examples/pvisor/07-vm-task)。

<a id="pause-resume"></a>
<a id="offload-resume"></a>

下一步：[暂停、卸载与恢复 VM](../guides/infrastructure/lifecycle.md) · [Runtime 与 Daemon 接入方式](../guides/infrastructure/integration.md)
