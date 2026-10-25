# Process smoke：最小 Run 与 Bundle 读取回归

Benchmark: B-PROCESS ([注册表](../REGISTRY.md#b-process)), role engineering A/B.
Motivation: 防止日常开发引入最小 Run、status 和 review 的进程级耗时回归。
Conclusion sought: 同机、同 harness 的候选版本与基线各条件中位数，默认 15% 回归门禁。
Design: 四个固定 argv，逐条件顺序运行；每条件 2 次预热、10 个正式样本，全部验证成功。

## Motivation

这组 smoke 用于 CI 和 PR，检测最小执行及读取已完成 Run 的成本变化。
它不代表真实应用、VM 启动、并发容量或配对随机实验，不能外推产品加速比例。

## Experiment design

计时使用 semaspec 公共运行库的 `perf_counter_ns`，从直接 argv 创建进程到正常零退出，
包含管道捕获；准备、Bundle 校验、解释器和统计处理在计时之外。保留全部慢样本。
预检创建一个独立 Run store 和已完成 Run，status/review 读取该 Run；host-run 每次
必须恰好新增一个成功完成的 Bundle，预热与正式样本使用相同校验。

准备时展开 `@BINARY@`、`@JOB_COMMAND@`、`@JOB@`、`@BATCH@`、`@REPO@`，
路径仍是独立 argv，不经过 shell 拼接。`@JOB_COMMAND@` 从 CLI 顶层 help 选择
spawn，兼容尚未重命名的基线 run。实验使用已准备的二进制，默认关闭附加性能日志。

```sh
mise run benchmark -- smoke
mise run benchmark -- smoke --binary target/release/pvisor --output benchmark/process/.data/smoke-new
```

Markdown 是四个条件的唯一声明；准备器通过 `semaspec filter --format json` 读取已验证
的计划，生成私有批次输入，再由 `semaspec bench` 和公共运行库执行。不要直接执行含
占位符的源文档。`mise run semaspec -- filter benchmark/process/smoke.md` 仅渲染规格。

### native

基准为 `/usr/bin/true` 的正常进程创建与退出。

<!-- semspec: bench id=native -->
```toml
argv = ["/usr/bin/true"]
conclude = ["true"]
warmup = 2
runs = 10
timeout = 120
[env]
PVISOR_RUN_HOME = "@BATCH@/runs"
PVISOR_STARTUP_TIMING = "0"
PVISOR_FS_PROFILE = "0"
```

### host-run

每次执行最小 host Job，并在计时外验证恰好新增一个成功完成的 Run Bundle。

<!-- semspec: bench id=host-run -->
```toml
argv = ["@BINARY@", "@JOB_COMMAND@", "--no-agent-defaults", "--overlaynet", "off", "--stdio", "capture", "--", "/usr/bin/true"]
conclude = ["python3", "@REPO@/benchmark/process/check.py", "new-bundle", "@BATCH@/runs"]
warmup = 2
runs = 10
timeout = 120
[env]
PVISOR_RUN_HOME = "@BATCH@/runs"
PVISOR_STARTUP_TIMING = "0"
PVISOR_FS_PROFILE = "0"
```

### status

读取预检创建的已完成 Run 的 JSON 状态，计时外重新验证 Bundle。

<!-- semspec: bench id=status -->
```toml
argv = ["@BINARY@", "status", "--json", "@JOB@"]
conclude = ["python3", "@REPO@/benchmark/process/check.py", "bundle", "@JOB@/run-bundle.json"]
warmup = 2
runs = 10
timeout = 120
[env]
PVISOR_RUN_HOME = "@BATCH@/runs"
PVISOR_STARTUP_TIMING = "0"
PVISOR_FS_PROFILE = "0"
```

### review

读取同一个已完成 Run 的 JSON review，计时外重新验证 Bundle。

<!-- semspec: bench id=review -->
```toml
argv = ["@BINARY@", "review", "--json", "@JOB@"]
conclude = ["python3", "@REPO@/benchmark/process/check.py", "bundle", "@JOB@/run-bundle.json"]
warmup = 2
runs = 10
timeout = 120
[env]
PVISOR_RUN_HOME = "@BATCH@/runs"
PVISOR_STARTUP_TIMING = "0"
PVISOR_FS_PROFILE = "0"
```

## Data and analysis

`mise run benchmark -- smoke` 审核成功后输出四行 Markdown 表格，并生成 `smoke.render.json`。
默认临时证据在审核后清理；`--output` 显式保留新 `.data/` 批次，包括冻结规格、
公共 runtime、二进制和配置摘要、所有预热与正式记录、原始样本和 semaspec 导出。
任何命令、校验、制品摘要、样本数量或统计复算失败都阻止发布，旧结果不被覆盖。

CI 用相同候选 harness 测量基线和候选，逐条件比较中位数，默认阈值 15%。
两批的采样、平台、亲和性、计时器和 harness 必须一致；这是顺序独立批次的回归门禁，
不是因果加速或置信区间结论。本规格不预填成绩，结果来自实际成功批次。
