<a id="vm"></a>

# 在 VM 中运行工具 {#first-task}

执行真实 `qutebrowser-667` SWE 轨迹片段中的针对性测试和语法检查，再将结果交回调用方。无需配置模型 API。

示例应用候选补丁，在同一个 VM 和工作区中运行两条工具命令，批次结束后统一返回结果。

## 1. 准备环境 {#prerequisites}

按 [VM 前置条件](installation.md#vm-and-oci)准备 **Linux x86_64 节点**，在该节点的仓库根目录运行示例；尚未获取仓库时：

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


<a id="framework-integration"></a>

## 3. 将反馈交回调用方 {#collect-results}

`run.py` 准备任务并处理结果，`executor.py` 通过 CLI 执行任务。两者通过以下调用连接：

```python
feedback = execute(task, EXAMPLE / "candidate.patch")
```

替换任务镜像、候选补丁和工具命令，再将反馈交给你的 Agent、评测器或训练数据收集流程。调用逻辑与适配器也可以放入框架的 Worker 中。

示例中的 `execute` 是一个同步执行固定命令批次的适配函数，不包含远程调度或根据反馈生成下一次调用的 Agent 循环。

任务格式、错误处理和清理规则见[示例 README](https://github.com/DeepLink-org/pvisor/tree/main/examples/pvisor/07-vm-task)。

<a id="pause-resume"></a>
<a id="offload-resume"></a>

下一步：[暂停、卸载与恢复 VM](../guides/infrastructure/lifecycle.md) · [Runtime 与 Daemon 接入方式](../guides/infrastructure/integration.md)
