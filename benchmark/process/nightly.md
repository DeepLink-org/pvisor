# Process nightly：较大样本回归检查

Benchmark: B-PROCESS ([注册表](../REGISTRY.md#b-process)), role engineering A/B.

## Motivation

用更多统计样本检查最小 Session、status 和 review 的进程级回归；不作为用户性能排名。

## Experiment design

实现与正确性门禁全部复用 [`process.rs`](../criterion/src/process.rs)，条件及计时边界
见 [smoke](smoke.md)。nightly 每条件先执行至少 10 个正确性预热，并由 Criterion 0.7.0
进行 Flat 采样，50 个统计样本。校准、额外预热及多操作样本使真实命令次数不等于 50。
与历史固定 10/50 的 Python cohort 分开保留。

```sh
mise run benchmark -- nightly --binary target/release/pvisor \
  --output benchmark/process/.data/new-nightly
```

## Data and analysis

Criterion 负责估计与基线比较；Rust 保存每次操作、失败、Bundle 校验和来源证据。
不删除有效慢值、不把失败当零、不自动发布数字。精确证据布局见 [Criterion README](../criterion/README.md)。
