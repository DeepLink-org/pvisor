# Process nightly：较长采样的进程级回归检查

Benchmark: B-PROCESS ([注册表](../REGISTRY.md#b-process)), role engineering A/B.
Motivation: 用较长采样检查最小 Run、status 和 review 的版本回归。
Conclusion sought: 同机同 harness 的逐条件中位数，默认 15% 阈值。
Design: 四个固定 argv；每条件 10 次预热、50 个正式样本，逐次验证，不删除慢样本。

## Motivation

nightly 使用与 [smoke](smoke.md) 相同的负载和正确性要求，以独立采样配置运行。
它不提供配对改善比例、尾延迟保证或真实应用性能结论。

## Experiment design

计时边界、隔离 Run store、CLI 兼容探测和 Bundle 校验遵循 [smoke 协议](smoke.md#experiment-design)。
以下四个 bench cell 明确给出完整命令和参数；路径占位符在准备阶段展开，不经 shell 解释。

```sh
mise run benchmark -- nightly --binary target/release/pvisor --output benchmark.new/process/.data/nightly-new
```

<!-- semspec: bench id=native -->
```toml
argv = ["/usr/bin/true"]
conclude = ["true"]
warmup = 10
runs = 50
timeout = 120
[env]
PVISOR_RUN_HOME = "@BATCH@/runs"
PVISOR_STARTUP_TIMING = "0"
PVISOR_FS_PROFILE = "0"
```

<!-- semspec: bench id=host-run -->
```toml
argv = ["@BINARY@", "@JOB_COMMAND@", "--no-agent-defaults", "--overlaynet", "off", "--stdio", "capture", "--", "/usr/bin/true"]
conclude = ["python3", "@REPO@/benchmark.new/process/check.py", "new-bundle", "@BATCH@/runs"]
warmup = 10
runs = 50
timeout = 120
[env]
PVISOR_RUN_HOME = "@BATCH@/runs"
PVISOR_STARTUP_TIMING = "0"
PVISOR_FS_PROFILE = "0"
```

<!-- semspec: bench id=status -->
```toml
argv = ["@BINARY@", "status", "--json", "@JOB@"]
conclude = ["python3", "@REPO@/benchmark.new/process/check.py", "bundle", "@JOB@/run-bundle.json"]
warmup = 10
runs = 50
timeout = 120
[env]
PVISOR_RUN_HOME = "@BATCH@/runs"
PVISOR_STARTUP_TIMING = "0"
PVISOR_FS_PROFILE = "0"
```

<!-- semspec: bench id=review -->
```toml
argv = ["@BINARY@", "review", "--json", "@JOB@"]
conclude = ["python3", "@REPO@/benchmark.new/process/check.py", "bundle", "@JOB@/run-bundle.json"]
warmup = 10
runs = 50
timeout = 120
[env]
PVISOR_RUN_HOME = "@BATCH@/runs"
PVISOR_STARTUP_TIMING = "0"
PVISOR_FS_PROFILE = "0"
```

## Data and analysis

成功批次保存 240 次预热/正式记录、完整原始样本、冻结规格与制品摘要，重新审核所有
Bundle、退出和统计后才输出 `nightly.render.json`。任何失败都阻止覆盖旧结果。
nightly 只能与相同采样、平台、亲和性、计时器及 harness 的基线比较；不能与 smoke 混比。
原始证据按 `--output` 保留，历史数据及其他采样配置不合并。
