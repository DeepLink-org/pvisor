# Replay：原生轨迹准备的一致性与成本

Benchmark: B-REPLAY ([注册表](../REGISTRY.md#b-replay)), role user-facing.
Motivation: 判断支持的轨迹格式能否忠实还原历史前缀，且准备阶段没有工具执行或工作区副作用。
Conclusion sought: 七种适配器各自的准备正确性与固定合成轨迹的完整 CLI 耗时。
Design: 每个适配器固定 task 0，截取一个完整工具批次；Criterion 管理采样，逐次校验。

## Motivation

轨迹准备必须保留历史工具参数和观测，且不能执行工具、启动 Agent 或改写真实工作区。
这组实验同时验证这些性质并记录完整 CLI 成本；它不替代历史二十条轨迹的一致性覆盖，
也不代表真实任务、模型推理或继续执行的时间。

## Experiment design

测量直接启动原生 replay CLI 至退出的单调时钟时间，作为原生轨迹准备成本。
准备成本表示解析轨迹、验证边界及生成原生上下文制品的完整命令开销。
在相同的轨迹和运行条件下，成本越低，用户等待准备完成的时间越短。

Rust 实现位于 `../criterion/src/replay/`，公开注册入口为
`pub fn register(&mut Criterion)`。Criterion 0.7.0 的 `iter_custom` 只接收
`std::time::Instant` 在直接 `Command::spawn` 至 `Child::wait` 返回之间的耗时之和。
stdout/stderr 直接写入逐次证据文件，因此命令写日志的成本在计时内；文件创建、
轨迹和二进制摘要检查、JSON 解析、正确性校验、归档及超时 supervisor 创建不计时。
直接调用被测命令，不经过 shell、Python 或 `taskset` 包装。

七个固定条件是 `claude-code`、`codex`、`opencode`、`mini-swe-agent`、
`openhands`、`pi-agent` 和 `swe-agent`。每个条件使用原生 task 0 轨迹：
第一条命令为 `printf fixture-0 > marker-0.txt`，下一条为 `cat marker-0.txt`，
历史观测为 `historical observation`。保留原始两步轨迹、消息关系、版本字段和
JSON/JSONL 格式；只准备 `--after-step 1` 指定的第一个完整工具批次。
真实工作区只含 `existing` sentinel。输入和参数在 Rust 中生成，不调用 Python fixture 或 hooks。

每个条件的直接 argv 为以下结构，各占位项作为独立参数传递：

```text
FROZEN_BINARY --agent AGENT --trajectory FIXTURE/trajectory.json
  --after-step 1 --prepare-only --workspace FIXTURE/workspace
  --state-dir FIXTURE/state --output-dir FIXTURE/output
```

每次执行前要求源摘要和冻结二进制摘要未变、工作区 sentinel 和目录清单正确、
state/output 不存在。执行后要求 result 明确记录零工具执行、Agent 未启动、
prepared/verified、零 continued steps 和 after_step 1；manifest 必须指向正确
adapter/source、恰好一个 batch 和一个 tool call，且声明完整工具批次。
工具参数必须精确匹配第一条命令；原生 prefix 必须保留该命令和历史观测，
不能包含下一条命令。最后再次核对工作区、轨迹和冻结二进制。
失败停止运行，不剔除有效慢值。

每个条件有一次独立的 30 秒预检；Criterion 中每次命令上限 120 秒。
Unix 子进程使用独立进程组；超时杀死整组并等待直接子进程退出，正常退出后也清理
该组内遗留后代。失败和超时的部分 state/output、stdout/stderr 及错误记录仍保留。
不支持此 supervision 的非 Unix 平台明确失败，不退化为无界命令。

Criterion 的统计样本数设为 60、Flat 采样。预热和校准由 Criterion 管理；
一次统计样本可以包含多次命令。这里不宣称零预热、恰好 60 次命令或总共 420 次命令。
所有命令（包括预检、Criterion 预热和校准）都有独立的原始记录。
七个条件独立顺序测量，不宣称随机配对或跨适配器的因果性能排名。

共享入口提供预构建的 `PVISOR_BENCH_BINARY` 和全新证据目录
`PVISOR_BENCH_OUTPUT`；`PVISOR_BENCH_FAMILY=replay` 选择本模块，
`PVISOR_BENCH_ENTRY=replay/replay.md` 标识本规格。未指定输出时，模块在
`benchmark/replay/.data/criterion-<time>-<pid>/` 保留证据。二进制仅按字节摘要标识，
没有 source-build receipt 时不能声称它来自当前源码。共享入口集成见
[README](README.md#native-preparation-b-replay)。

## Data and analysis

本文件没有测量数据。新的 Criterion cohort 与历史 hyperfine 和 Python
`perf_counter_ns` cohort 分开，不能合并统计或覆盖旧公开表。
历史数值、CSV、图表和来源记录保持不变；新公开结果需要独立登记匹配的发布绑定。

每个新批次保存冻结二进制、Rust harness 源码和摘要、fixture/protocol、
命令 argv/cwd、宿主信息及实际 CPU affinity（Linux process status）。
`trials/AGENT/NNNNNNNN/operation.json` 保存逐次意图、命令耗时、退出状态、
超时及错误；同目录保存 stdout/stderr、原生 output/state 和成功时的 `audit.json`。
正式统计来自 Criterion；原始 operation 记录是正确性和逐次成本证据，
不能把含预检和校准的记录总数当作 Criterion 的统计样本数。
