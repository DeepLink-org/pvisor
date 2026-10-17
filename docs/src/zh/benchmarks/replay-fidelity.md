# 历史轨迹能否准确准备为新的执行起点？

## 主要结论 {#conclusions}

**新框架中，七个适配器的固定 task 0 各通过 60/60 次校验，完整 prepare-only CLI 时间的中位数为 5.56–5.74 ms；不执行工具，不修改工作区。下方 20 条轨迹的历史分簇结果属于不同输入集合，保留原口径；真实模型继续执行的一致率仍未测量。**

| 需求 | 选型含义 |
|---|---|
| 准备跨 Agent 的历史工具上下文 | 使用经过校验的固定格式适配器 |
| 恢复实际文件状态 | 单独恢复任务环境；前缀准备不重放文件修改 |
| 比较原生 resume 或 RL 管线的速度 | 尚无同轨迹计时对照，不能据此排名 |

<!-- measurement-status:start -->

本页新增实验与历史数据分开发布；各表适用的计时范围和输入见实验设计与来源，不合并不同批次。

<!-- measurement-status:end -->

## Motivation {#motivation}

训练和复现需要在准确的历史边界继续任务。丢失工具观测或混入下一步动作，会改变模型看到的上下文；准备时重新执行工具，还可能产生额外副作用。这决定了历史轨迹能否作为可靠的任务输入。

## 实验设计 {#interpretation}

每个适配器使用 20 条合成原生格式轨迹，每条有两个工具批次，选择 `after-step=1`、`prepare-only`。重复三次，每次用固定种子打乱适配器与任务顺序，共 420 次。固定格式版本见表，与安装的 Agent CLI 版本分开。

有效样本必须保留源轨迹摘要、完整第一批的命令参数和历史观测，排除第二批动作，报告 `replayed_tool_calls=0`、Agent 未启动，且工作区中预置文件的内容和文件集合不变。独立发布审计复查全部保留的命令、原生前缀和工作区证据。失败单列，不当作零耗时。

环境是 Linux/x86_64、AMD Ryzen 7 9700X、内核 7.2.8-200.fc44；进程固定到 CPU 0、1，没有 benchmark 专用宿主内存上限。宿主缓存不清空，replay 不执行预热，保留全部有效慢样本，没有事后干扰剔除。计时从 CLI 启动到 prepare-only 退出，包含进程启动和收尾，排除输入生成及事后审计；不发生模型请求或工具执行。

原生 Agent resume 和 RL 管线的前缀准备是相关选项，但没有作为同轨迹、同预算的计时对照。实验验证固定格式的准备契约，不验证完整会话恢复、模型下一动作或 reward 一致率。

## 实验数据和分析 {#results}

测量日期：2026-10-06。每个适配器 N=60，全部通过；单位 ms。分离簇分别列比例和中位数，P95 为整个适配器分布的观察参考，不承诺稳定尾延迟。

| 适配器 | 固定格式版本 | 通过 / 计划（失败） | 较低簇：比例；中位数 ms | 较高簇：比例；中位数 ms | P95 参考 ms |
|---|---|---:|---|---|---:|
| claude-code | claude-code/2.1.220/native-resume-v1 | 60/60 (0) | 52/60 (86.7%); 6.51 | 8/60 (13.3%); 16.24 | 17.07 |
| codex | codex/0.149.0/native-responses-jsonl-v1 | 60/60 (0) | 49/60 (81.7%); 6.59 | 11/60 (18.3%); 14.91 | 17.82 |
| opencode | opencode/1.17.7/native-events-jsonl-v1 | 60/60 (0) | 51/60 (85.0%); 6.52 | 9/60 (15.0%); 15.20 | 15.31 |
| mini-swe-agent | mini-swe-agent/2.4.6/native-messages-v1 | 60/60 (0) | 60/60 (100%); 6.75 | — | 14.84 |
| openhands | openhands/0.53.0/native-replay-v1 | 60/60 (0) | 45/60 (75.0%); 6.22 | 15/60 (25.0%); 11.44 | 15.01 |
| pi-agent | pi-agent/0.83.0/native-rpc-events-v1 | 60/60 (0) | 47/60 (78.3%); 6.07 | 13/60 (21.7%); 11.15 | 15.07 |
| swe-agent | swe-agent/1.1.0/replay-then-live-v1 | 60/60 (0) | 51/60 (85.0%); 6.55 | 9/60 (15.0%); 14.97 | 15.20 |

Mini-SWE-Agent 未触发分簇规则，该行列出全体样本的 P50。分簇是描述性判定：两簇各至少占 10%，最大相邻间隔至少为整体中位数的 20%、且超过相邻间隔中位数的三倍，两簇中位数至少相差 1.5 倍；它不证明慢簇的成因。有效慢样本全部保留，因此不宜把准备成本概括为固定的几毫秒。

### Hyperfine：直接执行 prepare-only 命令 {#hyperfine}

独立批次为每个适配器固定一条合成轨迹（task 0），采样 60 次，无预热。hyperfine 使用 `--shell=none` 直接启动 `pvisor-replay`；Python 重置和一致性校验在每次计时外执行。所有执行必须保留精确参数、源摘要与历史观测，不执行工具、不改动工作区。数值表示完整命令结束时间，适用范围是这条固定轨迹；上方二十条轨迹的历史分布保留原口径。二进制摘要标识实测制品，与当前源码的构建对应关系未验证。保留宿主缓存；CPU 亲和性和制品标识见批次来源摘要，不据此推导配对改善比例或尾延迟保证。

{{ table("../../../overrides/assets/benchmarks/replay-semspec.render.json") }}

[Hyperfine 数据 CSV](../../../overrides/assets/benchmarks/data/replay/replay-semspec.csv) · [批次来源](../assets/benchmarks/replay-semspec.provenance.json) · [JSON benchmark 复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark.new/README.md#replay)

### 适用边界 {#acceptance}

这些结果支持固定短前缀的结构、参数和观测保真。真实会话、新版 CLI、长前缀、token 成本、macOS、远端连接恢复及后续模型执行未测。需要工具重放或实际环境恢复时，应另行验证其正确性和成本。

### 来源与复现 {#run}

完整 CSV/JSON 和原始记录保存在对应本地批次或文档数据归档。

[比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
