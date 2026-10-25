# 状态保存与分支复用

<span id="state-reuse-and-resource-optimization"></span>

Agent 在已有工作基础上尝试另一种方案时，需要明确保存哪些状态、从哪里继续，以及如何隔离不同尝试的修改。pVisor 通过检查点保存指定范围的状态，通过分支建立独立的后续执行路径。

## 先确定要保存什么 {#state-scope}

| 状态 | 保存与复用的内容 | 主要边界 |
| --- | --- | --- |
| 工作区检查点 | 暂存文件变化、冲突基线及来源信息 | 不保存进程内存；基础文件仍可能是外部引用 |
| VM 执行检查点 | 支持配置下的一致 CPU、RAM、设备与文件系统状态 | 依赖平台、配置与兼容性；不回退外部服务 |
| Agent 轨迹 | 记录的模型交互与工具调用，供回放流程使用 | 不等同于完整机器状态；工具执行仍可能产生副作用 |

工作区分支从保存的候选文件变化启动新命令；执行分支从完整执行检查点继续机器执行。两者都需要明确来源状态和独立写入范围。当前命令与支持配置以 [CLI 参考](../reference/cli.md#full-vm-execution-checkpoints)为准；轨迹的使用见[回放历史轨迹](../guides/replay.md)。

## 保持分支独立 {#shared-bases}

分支关联来源检查点，后续修改进入各自的文件写层；支持的执行恢复配置也保持 RAM 写入私有。这样可以从相同起点继续不同方案，并分别检查结果。工作区检查点的基础文件可能仍指向外部路径，因此保存 upper 不等于保存整个环境的不可变副本。

执行检查点需要同时保存一致的 CPU、RAM、设备与文件状态，并满足恢复兼容性。它不会让远程 API 或数据库随本地状态回退。支持配置见 [CLI 参考](../reference/cli.md#full-vm-execution-checkpoints)，一致性机制见[环境快照](../design/environment-snapshot.md)。

## 保存状态与继续执行 {#lifecycle-and-cost}

在支持的原生 VM 配置下，创建执行检查点后可以继续运行；`suspend` 保存检查点并等待原 VM 退出，`resume` 恢复当前挂起状态，执行分支则使用独立 Job。运行时对存活 VM 的暂停和卸载另有生命周期，见[沙箱生命周期管理](lifecycle.md)。

<span id="resource-mechanisms"></span>

保存状态后的按需加载、共享和压缩机制见[镜像与内存优化](resource-optimization.md#resource-mechanisms)。状态是否可恢复与资源能节省多少，需要分别核对。
