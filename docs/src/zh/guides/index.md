# Agent style stagebox：从实验到成果接收

先通过[离线首跑](../start/first-run.md#agent-style)熟悉暂存。用于已有项目时，给 Agent 一个可以检查的任务，并在目标环境验证接收的文件。

<div class="pv-guide-steps" markdown="1">

1. ### 准备一个可检查的任务 {#prepare}

    先完成[安装与平台准备](../start/installation.md)，再用[第一次运行](../start/first-run.md)中的离线任务验证暂存和审查。演示不需要模型账号，可以直接比较宿主工作区与 Job 中的候选文件。

    接入真实 Agent 时，按[Agent 接入指南](agents/index.md)确认程序安装位置、凭据和所需网络目标。准备好这些条件后，再把离线命令换成 Agent 或自己的脚本。

    给任务明确的目标和接受条件，例如“修复这个测试，允许修改源码和测试，保留其他文件”。记录开始时的仓库状态，后续审查才有可比较的基线。

    原生 Job 使用 `pvisor spawn` 启动；`pvisor run` 用于独立容器命令，不沿用原生 Job 的暂存与审查入口。具体用法见[CLI 参考](../reference/cli.md)。

1. ### 选择执行边界 {#boundaries}

    根据工具需要选择[执行器](executors/index.md)，分别配置[文件访问](policies/files.md)、[网络出口](policies/network.md)与[凭据](policies/credentials.md)。查看实际安装的控制，不能由“有暂存文件”推断网络或读取已经隔离。

    需要事后接受文件改动时，显式选择暂存：`--safe` 请求预设控制并暂存工作区，`--stage PATH` 指定暂存位置。普通 host 执行默认直写。各位置的写入与清理规则见[CLI 暂存与存储](../reference/cli.md#暂存与存储)。

    使用自定义 stage 时，为每次任务选项目外的新目录。后续命令通过该路径或明确的 Job 身份定位结果；`last` 的查找范围见[Job 与存储](../concepts/jobs.md)。

1. ### 运行一个有明确验收条件的任务 {#run-task}

    在目标项目根目录打开终端，先记录已有改动：

    ```bash title="Shell"
    git status --short
    ```

    保留这些改动作为基线。按 [Agent 接入](agents/index.md)完成认证和网络配置后，启动 Agent，例如：

    ```bash title="Shell"
    pvisor spawn --safe --pass-env ANTHROPIC_API_KEY -- claude
    ```

    给 Agent 一个可检查的任务，例如“修复指定失败测试，运行对应测试，只修改相关源码和测试”。等待任务完成并退出 Agent 后，再执行 review。模型认证失败、工具退出非零或用户取消时，先查看 `pvisor status last` 与 `pvisor review last --diff`。记录不完整，不能据此判断没有改动；请按[故障排查](troubleshooting.md)处理。

1. ### 执行完成后怎样决定 {#review}

    任务结束后，先核对执行结果、退出码和实际控制，再检查净文件改动。命令成功不代表修改满足任务要求；非零退出也可能留下有用的候选。

    | 看到的结果 | 下一步 | 需要保留什么 |
    | --- | --- | --- |
    | 修改符合要求 | 选择路径合入；处理尚未接受的部分 | 运行证据与最终接受结果 |
    | 部分修改有用 | 分批接受，或在清理前保存文件分支 | 未合入候选与冲突基线 |
    | 执行失败或改动不合适 | 先定位失败，再决定丢弃候选或重新运行 | 失败诊断和必要的复现输入 |
    | 宿主文件已被其他操作修改 | 检查冲突并明确解决 | 当前宿主文件与原 stage |

    具体命令、只读检查和冲突恢复见[审查并应用改动](review-apply.md)。`drop` 处理未接受的文件候选；外部调用和已合入批次的范围见[暂存语义](../concepts/staging.md)。

    想保留文件方案再试一种做法时，先检查[工作区分叉](fork-checkpoint.md)的前提。需要继续 CPU、RAM 和设备状态时，选择[执行检查点入口](infrastructure/integration.md#capabilities)。

1. ### 验证并交接成果 {#handoff}

    先检查文件差异，再从只读任务视图查看需要的文件：

    ```bash title="Shell"
    pvisor review last --diff
    pvisor inspect last -- git diff --check
    ```

    上面的 Git 检查适用于 Git 项目，只检查格式问题，不能代替功能测试。`inspect` 视图只读；需要写缓存或产物的测试应在任务执行期间运行，并在接收后到目标环境再次验证。

    检查哪些改动应进入项目、哪些只是实验残留，以及源码、测试和配置之间是否存在依赖。review 展示文件变化，分类由你判断，也不会认证文件是否安全。按项目中实际存在的路径接收，例如源码与测试都需要时：

    ```bash title="Shell"
    pvisor apply last --path src --path tests
    git diff --stat
    ```

    随后在原项目运行自己的测试命令。部分接收可能遗漏依赖或配置；目标环境验证失败时，先检查尚未接收的变化，保留 stage 再决定。合入不是 Git 提交，已有工作区改动也需要一并审查。

    确认剩余候选均不需要后才执行 `pvisor drop last`；如果已接受全部变化，暂存流程已结束，无需再 drop。drop 不撤销已接受的文件。需要重试时，为新任务准备独立 stage；想保留文件方案再分叉，按[分叉前提](fork-checkpoint.md)操作。工作区分叉不等于恢复 VM 执行状态。

</div>

## 轨迹记录与回放 {#trajectories}

本地任务需要追踪模型交互时使用[轨迹捕获](capture.md)，需要重建受支持的 Agent 历史并继续执行时使用[回放](replay.md)。同一套指南也用于推理与评测中的[上下文保留](infrastructure/index.md#inference)；需要将执行结果交付训练流程时，再参考 [RL rollout](rl-rollouts.md)。

## 扩展到并行任务与 CI {#automation}

并行任务分别使用独立工作区、stage 和 Job 身份。按[并行 Agent 流程](parallel-agents.md#batch)收集各任务结果，再协调合入同一个目标树；任务数增加后，接受条件仍由你或评审流程确定。

[CI 工作流](ci.md)把运行结果、产物上传和接受改动分开处理。保留失败与超时的证据，使评审步骤能判断是否重跑、修复或丢弃。

失败可按[排查一次 Run](troubleshooting.md)定位。

当任务由推理服务、评测器或训练器发起，使用[Agent 执行基础设施](infrastructure/index.md)确定状态和资源由谁管理；两条流程共享相同的执行器与能力说明。
