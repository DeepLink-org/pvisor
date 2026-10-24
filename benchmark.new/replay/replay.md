# Replay：原生轨迹准备的一致性与成本

Benchmark: B-REPLAY ([注册表](../REGISTRY.md#b-replay)), role user-facing.
Motivation: 判断支持的轨迹格式能否忠实还原历史前缀，且准备阶段没有工具执行或工作区副作用。
Conclusion sought: 七种适配器各自的准备正确性与固定合成轨迹的完整 CLI 耗时。
Design: 每个适配器固定 task 0，截取一个完整工具批次；零预热、60 个正式样本，逐次校验。

## Motivation

轨迹准备必须保留历史工具参数和观测，且不能执行工具、启动 Agent 或改写真实工作区。
这组实验同时验证这些性质并记录完整 CLI 成本；它不替代历史二十条轨迹的一致性覆盖，
也不代表真实任务、模型推理或继续执行的时间。

## Experiment design

使用 semspec 公共运行库的 `perf_counter_ns`，计时直接 argv 创建进程至正常零退出，
包括命令本身和管道捕获；外层 Python、准备/校验 hooks、归档和统计不计时。
七种适配器逐条件顺序采样，不宣称随机配对或跨适配器的因果性能排名。

准备器冻结二进制与原生轨迹，并对每种格式单独预检。每个正式样本前核对源摘要、
工作区 sentinel 及空 state/output；之后要求零工具执行、Agent 未启动、准确的完整
批次边界、正确参数及历史观测，且不含边界后的动作。再次核对真实工作区与源摘要，
校验成功才归档输出和状态；失败停止该批次，完整保留失败与慢样本。

```sh
mise run benchmark -- replay --binary target/release/pvisor-replay
mise run benchmark -- replay --binary target/release/pvisor-replay --output benchmark.new/replay/.data/replay-new
```

Markdown 中的 bench cell 是唯一命令定义。准备器经 `semspec filter --format json`
读取计划，展开 `@BINARY@`、`@BATCH@` 和 `@REPO@`，生成批次输入；然后由
`semspec bench` 执行并审计。占位符是独立 argv，不经过 shell 解释。
未展开的源文档仅供只读渲染，不直接使用 `filter --execute` 测量。

### claude-code

<!-- semspec: bench id=claude-code -->
```toml
argv = [
  "@BINARY@", "--agent", "claude-code",
  "--trajectory", "@BATCH@/fixtures/claude-code/trajectory.json",
  "--after-step", "1", "--prepare-only",
  "--workspace", "@BATCH@/fixtures/claude-code/workspace",
  "--state-dir", "@BATCH@/fixtures/claude-code/state",
  "--output-dir", "@BATCH@/fixtures/claude-code/output",
]
prepare = ["python3", "@REPO@/benchmark.new/replay/replay_hooks.py", "prepare", "@BATCH@", "claude-code"]
conclude = ["python3", "@REPO@/benchmark.new/replay/replay_hooks.py", "conclude", "@BATCH@", "claude-code"]
warmup = 0
runs = 60
timeout = 120
```

### codex

<!-- semspec: bench id=codex -->
```toml
argv = [
  "@BINARY@", "--agent", "codex",
  "--trajectory", "@BATCH@/fixtures/codex/trajectory.json",
  "--after-step", "1", "--prepare-only",
  "--workspace", "@BATCH@/fixtures/codex/workspace",
  "--state-dir", "@BATCH@/fixtures/codex/state",
  "--output-dir", "@BATCH@/fixtures/codex/output",
]
prepare = ["python3", "@REPO@/benchmark.new/replay/replay_hooks.py", "prepare", "@BATCH@", "codex"]
conclude = ["python3", "@REPO@/benchmark.new/replay/replay_hooks.py", "conclude", "@BATCH@", "codex"]
warmup = 0
runs = 60
timeout = 120
```

### opencode

<!-- semspec: bench id=opencode -->
```toml
argv = [
  "@BINARY@", "--agent", "opencode",
  "--trajectory", "@BATCH@/fixtures/opencode/trajectory.json",
  "--after-step", "1", "--prepare-only",
  "--workspace", "@BATCH@/fixtures/opencode/workspace",
  "--state-dir", "@BATCH@/fixtures/opencode/state",
  "--output-dir", "@BATCH@/fixtures/opencode/output",
]
prepare = ["python3", "@REPO@/benchmark.new/replay/replay_hooks.py", "prepare", "@BATCH@", "opencode"]
conclude = ["python3", "@REPO@/benchmark.new/replay/replay_hooks.py", "conclude", "@BATCH@", "opencode"]
warmup = 0
runs = 60
timeout = 120
```

### mini-swe-agent

<!-- semspec: bench id=mini-swe-agent -->
```toml
argv = [
  "@BINARY@", "--agent", "mini-swe-agent",
  "--trajectory", "@BATCH@/fixtures/mini-swe-agent/trajectory.json",
  "--after-step", "1", "--prepare-only",
  "--workspace", "@BATCH@/fixtures/mini-swe-agent/workspace",
  "--state-dir", "@BATCH@/fixtures/mini-swe-agent/state",
  "--output-dir", "@BATCH@/fixtures/mini-swe-agent/output",
]
prepare = ["python3", "@REPO@/benchmark.new/replay/replay_hooks.py", "prepare", "@BATCH@", "mini-swe-agent"]
conclude = ["python3", "@REPO@/benchmark.new/replay/replay_hooks.py", "conclude", "@BATCH@", "mini-swe-agent"]
warmup = 0
runs = 60
timeout = 120
```

### openhands

<!-- semspec: bench id=openhands -->
```toml
argv = [
  "@BINARY@", "--agent", "openhands",
  "--trajectory", "@BATCH@/fixtures/openhands/trajectory.json",
  "--after-step", "1", "--prepare-only",
  "--workspace", "@BATCH@/fixtures/openhands/workspace",
  "--state-dir", "@BATCH@/fixtures/openhands/state",
  "--output-dir", "@BATCH@/fixtures/openhands/output",
]
prepare = ["python3", "@REPO@/benchmark.new/replay/replay_hooks.py", "prepare", "@BATCH@", "openhands"]
conclude = ["python3", "@REPO@/benchmark.new/replay/replay_hooks.py", "conclude", "@BATCH@", "openhands"]
warmup = 0
runs = 60
timeout = 120
```

### pi-agent

<!-- semspec: bench id=pi-agent -->
```toml
argv = [
  "@BINARY@", "--agent", "pi-agent",
  "--trajectory", "@BATCH@/fixtures/pi-agent/trajectory.json",
  "--after-step", "1", "--prepare-only",
  "--workspace", "@BATCH@/fixtures/pi-agent/workspace",
  "--state-dir", "@BATCH@/fixtures/pi-agent/state",
  "--output-dir", "@BATCH@/fixtures/pi-agent/output",
]
prepare = ["python3", "@REPO@/benchmark.new/replay/replay_hooks.py", "prepare", "@BATCH@", "pi-agent"]
conclude = ["python3", "@REPO@/benchmark.new/replay/replay_hooks.py", "conclude", "@BATCH@", "pi-agent"]
warmup = 0
runs = 60
timeout = 120
```

### swe-agent

<!-- semspec: bench id=swe-agent -->
```toml
argv = [
  "@BINARY@", "--agent", "swe-agent",
  "--trajectory", "@BATCH@/fixtures/swe-agent/trajectory.json",
  "--after-step", "1", "--prepare-only",
  "--workspace", "@BATCH@/fixtures/swe-agent/workspace",
  "--state-dir", "@BATCH@/fixtures/swe-agent/state",
  "--output-dir", "@BATCH@/fixtures/swe-agent/output",
]
prepare = ["python3", "@REPO@/benchmark.new/replay/replay_hooks.py", "prepare", "@BATCH@", "swe-agent"]
conclude = ["python3", "@REPO@/benchmark.new/replay/replay_hooks.py", "conclude", "@BATCH@", "swe-agent"]
warmup = 0
runs = 60
timeout = 120
```

## Data and analysis

成功批次输出每种格式的 N、中位数、均值、样本标准差、最小和最大值；保留纳秒样本，
不剔除有效慢值。`--output` 选择全新 `.data/` 目录，保存冻结规格、计时器、二进制与
轨迹摘要、7 次预检、420 次正式命令记录及对应的逐次原生前缀/工作区审计。
缺少 source-build receipt 时，二进制来源仅由字节摘要标识，不能声明对应当前源码。

旧公开表使用历史 hyperfine 口径，现有发布门禁拒绝用这里的 Python 时钟结果覆盖。
本规格迁移不改历史数值、CSV 或图表；新的公开 cohort 需要独立登记匹配的发布绑定。
