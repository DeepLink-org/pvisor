# 作为 Agentic RL rollout 与评测的执行层

把一次 rollout 看成独立任务：准备工作区，执行 Agent，收集轨迹和文件结果，计算奖励，然后保留或丢弃改动。pVisor 负责执行与证据，训练器负责采样、奖励和调度。

先用离线任务验证结果收集：

```bash
pvisor run --safe --overlaynet-deny-all --stdio capture \
  --stage ../rollout-001 -- /bin/sh -c 'printf "candidate\n" > answer.txt'
pvisor status --review --json ../rollout-001 > rollout-001.bundle.json
pvisor inspect ../rollout-001 -- cat answer.txt
```

结果应当是任务完成、退出码为零，并留下 `answer.txt`。让 evaluator 读取 Stage 中的候选文件，评分完成后执行 `pvisor drop ../rollout-001`；不需要把每个训练候选写回基线。

## 单次 rollout 的现有拼装方式

当前可以将一次命令执行、模型流量记录和原生 Agent 轨迹作为一个评测样本。先为样本准备干净 workspace，再运行命令并保留 Job ID、Run Bundle、Event Journal 与 Agent 原生轨迹。reward 的计算、任务队列、模型训练和跨节点调度由现有框架负责。

| 产物 | 用途 | 不能替代 |
| --- | --- | --- |
| Run Bundle | 结果、控制与文件变化 | 训练框架的 reward 与数据集元数据 |
| Gateway Journal | 经过网关的模型调用 | 未捕获流量与 Agent 原生 session |
| Agent 原生轨迹 | 对应适配器的工具前缀回放 | 进程内存快照 |
| 逻辑检查点 | 分叉暂存文件状态 | 完整环境和外部服务状态 |

## 固定样本身份

为每次 rollout 保存任务 ID、pVisor 提交、Agent/模型版本、初始仓库提交、镜像摘要、采样参数、策略和执行器。分别记录任务失败、隔离拒绝、记录失败与续跑质量；不要把基础设施失败记为“模型没有能力”。

回放在新环境中重新执行工具，会再次发生相应副作用。使用测试 API/数据库和独立输出目录；用 `--prepare-only` 先验证格式，用 `--replay-only` 验证工具前缀，再开始真实续跑。固定适配版本与边界语义见[回放](replay.md)。集群吞吐与 hostile 多租户仍未建立保证。

## 评分并保留训练样本 {#score}

前面的离线任务可用下面的最小 evaluator 评分。它先检查执行结果，再检查候选文件内容；示例依赖 `jq`。

```bash
jq -e '.schema_version == 4 and .run.state == "completed" and .run.exit_code == 0' \
  rollout-001.bundle.json
pvisor inspect ../rollout-001 -- cat answer.txt > rollout-001.answer.txt
reward=0
if [ "$(cat rollout-001.answer.txt)" = candidate ]; then
  reward=1
fi
jq -n --arg task_id sample-001 --argjson reward "$reward" \
  '{task_id: $task_id, reward: $reward, bundle: "rollout-001.bundle.json"}' \
  > rollout-001.score.json
pvisor drop ../rollout-001
```

这只是内容匹配奖励，真实任务替换为单元测试、环境得分或人工标注。若执行检查或文件读取失败，记录为失败样本并保留诊断，停止后面的正常评分步骤。CI 中使用 `set -e` 或显式检查每一步返回值。

收集原生 Agent 轨迹时按[接入指南](agents/index.md)配置 Agent 自身的输出位置；启用 Gateway 捕获的步骤见[模型通信记录](capture.md)。把轨迹位置、Bundle 与评分用同一个 task/attempt ID 关联。批量任务复用[并行工作区流程](parallel-agents.md#batch)，每个样本使用新的 Stage。
