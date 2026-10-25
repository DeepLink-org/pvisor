<a id="vm"></a>

# 执行基础设施：运行第一个任务 {#first-task}

在 Linux 执行节点运行仓库自带的 VM 任务示例，了解如何从调用方提交任务、收集结果。评测器、训练框架或调度程序可以沿用这一流程，无需先部署 Daemon。

## 1. 准备环境 {#prerequisites}

先完成[安装指南](installation.md#vm-and-oci)中的 Linux/KVM 节点配置，并激活安装 pVisor 的 Python 环境。以下命令均在该执行节点上运行。

示例由仓库直接提供，无需手工创建脚本。已有当前仓库时进入仓库根目录；尚未获取时：

```bash
git clone https://github.com/DeepLink-org/pvisor.git
cd pvisor
```

示例指定了包含任务依赖的 Python 镜像，pVisor 首次运行时自动拉取并缓存，不需要安装 Docker 或手工准备 rootfs。首次拉取需要节点能访问镜像仓库；离线准备方式见 [VM 执行器](../guides/executors/vm.md#rootfs)。

## 2. 运行示例 {#start-task}

```bash
bash examples/pvisor/07-vm-task/run.sh
```

示例读取输入 `20` 和 `22`，在 VM 中计算并写出结果。目录中的文件各司其职：

| 文件 | 职责 |
| --- | --- |
| `run.py` | Host 侧调用方：提交任务、检查执行状态、收集结果并清理 stage |
| `input.json` | 任务输入 |
| `task.py` | VM 内执行的任务程序 |

任务使用 1 vCPU、256 MiB guest RAM，关闭 guest 网络，不调用模型 API。

## 3. 查看结果 {#collect-results}

成功后，终端输出 `result.answer: 42`，以及本次运行的 `Records` 目录。该目录位于示例的 `.work/` 下，包含：

- `sample.json`：调用方任务 ID、Job／Attempt ID 和任务结果。
- `bundle.json`：执行状态、退出码与执行记录。

成功收集结果后释放 stage；执行失败时保留现场，方便检查。

接入自己的任务时，替换 `input.json` 和 `task.py`，再让调用方读取 `sample.json` 进行评分或后续调度。需要其他工具时修改示例的镜像配置；执行参数和结果收集逻辑集中在 `run.py` 中。

<a id="pause-resume"></a>
<a id="offload-resume"></a>

下一步：[沙箱生命周期管理](../concepts/lifecycle.md#cli-example) · [Runtime 与 Daemon 接入方式](../guides/infrastructure/integration.md)
