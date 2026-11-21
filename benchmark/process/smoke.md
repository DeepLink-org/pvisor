# Process smoke：最小 Session 与 Bundle 读取回归

Benchmark: B-PROCESS ([注册表](../REGISTRY.md#b-process)), role engineering A/B.

## Motivation

检测最小执行以及读取已完成 Session 的进程级成本变化。这不是 VM、真实应用、
并发容量或随机配对的性能排名。

## Experiment design

唯一实现是 [`process.rs`](../criterion/src/process.rs)。四个条件为原生
`/usr/bin/true`、host Session、status JSON、review JSON。Rust 直接调用 argv，
计时从启动到正常退出及管道 EOF；输入准备、Bundle/JSON 校验、摘要和证据写入不计时。
每次（包括预热和校准）必须成功；host-run 恰好新增一个成功 Bundle，读取条件不得
改变 Bundle 集合，二进制与工作区 sentinel 前后不变。失败保留记录并中止。

Criterion 0.7.0 使用 Flat 采样，50 个统计样本、每条件5秒目标测量预算；
每条件另有至少2个正确性预热，Criterion额外预热100ms。
Criterion 自适应预热、校准和每样本操作数，因此不再声称“恰好 10 次命令”。
新 cohort 不与历史 Python/hyperfine 样本混合。
该固定预算也不与此前10样本/1秒的Criterion批次合并；不删除慢值或反复测量择优。

```sh
mise run benchmark -- smoke --binary target/release/pvisor \
  --output benchmark/process/.data/new-smoke \
  --criterion-output benchmark/process/.data/criterion -- --save-baseline main
```

制品需预先构建。独立 Run store 和 workspace 由 Rust 创建；Session 命令从 CLI help
选择 spawn，兼容旧基线的 run。Markdown 不再包含可执行 benchmark 语言或 Python hooks。

## Data and analysis

Criterion 保留采样、估计和基线比较。`operations.jsonl` 保留逐操作原始时长，包含预检、
正确性预热和 Criterion 调用，不从批次均值推算操作尾延迟。原始命令记录、stdout/stderr、
来源摘要和冻结 harness 保存在新批次。默认 15% CI 门禁读取 Criterion 的 median estimate；
同机顺序 cohort 的门禁不是随机交替 A/B，不支持因果改善百分比。
`resources-*.json` 在操作计时区间外记录前后 boot ID、uptime、内存、swap 累计计数、
CPU/内存/I/O PSI 及 observer cgroup 配额和节流计数；不可读项显式保留错误。
跨 boot 或计数器重置不能计算速率，边界采样也不能证明区间内没有压力峰值。
原始慢样本不因这些观测被删除、重试或放宽阈值。

`--diagnose` 在原测量之外同时启用 `PVISOR_STARTUP_PROFILE=1` 与
`PVISOR_STARTUP_TIMING=1`，保留各诊断命令的资源边界。Host profile 后接 strace，
共用新的私有 Run store：前者包含初始化，后者使用已初始化 store，不视为配对样本。
strace wall time 包含调度和观测开销，不能直接解释为磁盘服务时间或验收结果。
实验不自动写用户文档或覆盖任何历史表。
