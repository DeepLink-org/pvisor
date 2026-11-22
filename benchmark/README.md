# pVisor benchmarks

这里回答的问题：如何用可审查的 Rust 实验，通过 Criterion 采样，并保留正确性与来源证据？
实验准入、角色与发布要求以 [REGISTRY.md](REGISTRY.md) 为准。

## 活动范围与入口

[registry.csv](registry.csv) 的 `implementation` 指向当前 Rust 实现。迁移保留原有键的身份；
新实验独立登记。33 个原未准备条目仍为
`requires-single-condition-command`，在执行任何实验之前拒绝测量。`criterion-rust`
表示已有实现，不表示当前机器具备 KVM、macOS、固件、镜像、委派 cgroup 等实验前提。
JSON 仅为非执行的定位信息，Markdown 仅说明设计；不再通过 Markdown/JSON 运行 Python
行命令。历史退役脚本不因仍存在于 checkout 而成为活动入口。
逐入口实现、smoke 与未验证环境清单见 [MIGRATION.md](MIGRATION.md)。

| 实验族 | 原活动键数 | Rust 实现与复现手册 |
| --- | ---: | --- |
| Process smoke/nightly | 2 | [`process.rs`](criterion/src/process.rs)、[smoke](process/smoke.md) |
| Ready/Exit、镜像对照与 daemon 预热池 | 12 | [`startup/`](criterion/src/startup/mod.rs)、[startup README](startup/README.md) |
| 原生文件、九运行时与 DAX | 3 | [`filesystem/`](criterion/src/filesystem/mod.rs)、[filesystem README](filesystem/README.md) |
| 网络 profile A/B | 1 | [`network/`](criterion/src/network/mod.rs)、[network README](network/README.md) |
| 七种轨迹格式准备 | 1 | [`replay/`](criterion/src/replay/mod.rs)、[replay README](replay/README.md) |
| allocator、scanner、pool、library | 4 | [`memory/`](criterion/src/memory/mod.rs)、[memory README](memory/README.md) |
| 镜像库存、格式、运行、本地/远程按需读取 | 7 | [`storage/`](criterion/src/storage/mod.rs)、[storage README](storage/README.md) |

```sh
mise run benchmark -- --list
mise run benchmark -- smoke --binary target/release/pvisor \
  --output benchmark/process/.data/new-smoke
mise run benchmark -- replay --binary target/release/pvisor-replay \
  --output benchmark/replay/.data/new-replay
mise run benchmark -- startup/native_timing \
  --output benchmark/startup/.data/new-native
```

制品、镜像和系统权限必须预先准备。入口不自动下载或重建被测产品，也不为未准备模板
猜测命令。专用协议的路径、预算、工具与 metric 配置见各实验族 README。
`--output` 必须是新目录；默认生成新的 `benchmark/.data/criterion-...`。
同名旧批次不复用。失败现场保留，不删除外部文件或无关服务。

## 测量边界与统计

锁定实际发布的 **Criterion.rs 0.7.0**，使用稳定公开 API。Criterion 负责自适应校准、
预热、采样、估计和基线比较。
Rust 实验控制测量边界：`Bencher::iter_custom` 返回对应 `iters` 次真实操作的总
`Duration`，准备、JSON 解析、正确性校验和证据写入不进入自报值。
Ready 与 Completion 分开，guest 内部阶段不替换成 CLI 全生命周期。

非时间指标使用自定义 `Measurement` / `ValueFormatter`，明确 B、计数及“一次操作”的
含义。逐操作内存窗口值/差值与 cgroup lifetime highwater 不混称为任务峰值。
原始请求/操作分布独立保存；Criterion 的多操作统计样本不是原始操作，
不能从批次均值推算操作 P95/P99，也不能把非加总指标伪装为纳秒或吞吐。

**这是新的统计 cohort。** Criterion 的统计样本数不等于 CLI 调用次数；校准与自适应预热
也会执行操作。旧固定 N3/N30/N60 等次数不再声称逐次兼容，必要正确性预热和最小统计
样本数由各实验保留并说明。完整随机 pair/矩阵仍在一次操作内执行，逐臂记录不丢失。
Criterion 默认基线区间不是随机配对中位数差的置信区间，不自动满足 REGISTRY 的
百分比、双峰或尾延迟发布要求。没有完成这些准入分析时，不发布因果改善结论。

每次预检、预热、校准和测量都校验输出、正常退出、来源与实验隔离条件。失败停止操作，
不作为零、不跳过失败 cell 来缩短矩阵。进程观察和资源所有者保留超时、清理及失败记录。

## 基线与 CI

```sh
mise run benchmark -- smoke --binary /prepared/main/pvisor \
  --output benchmark/process/.data/main \
  --criterion-output benchmark/process/.data/criterion -- --save-baseline main
mise run benchmark -- smoke --binary /prepared/candidate/pvisor \
  --output benchmark/process/.data/candidate \
  --criterion-output benchmark/process/.data/criterion -- --baseline main
mise run benchmark-compare -- benchmark/process/.data/criterion \
  --candidate new --output benchmark/process/.data/comparison
```

CI 并行构建两份产品，在同一测量机使用候选 harness 顺序执行基线和候选。
Criterion 保存原始统计与基线比较；Rust 的 `benchmark-compare` 只读取 Criterion
`median.point_estimate` 执行现有 15% 门禁，不另写采样器或统计器。这是工程回归门禁，
不是随机交替 A/B 的用户加速结论。构建 SHA/归档摘要、原始操作和独立诊断继续保留。
每轮完成后（包括门禁失败）另行保留 profile/persistence timing 和 strace，
诊断不替换原始采样或门禁结论；同时记录测量命令前后的宿主压力和 observer cgroup
约束。诊断 trace 的 syscall wall time 包含调度和观测开销，不等同于磁盘服务时间。

## 证据与历史展示

各族在新 `.data/` 中保存配置、来源/制品摘要、冻结源码、实际 argv、stdout/stderr、
退出状态、校验结果、逐操作时钟/资源观察与清理证据；Criterion 输出独立于操作记录。
默认 Criterion 目录可用 `--criterion-output` 指定，不向文档目录写实验成绩。
缺少 source-build receipt 时只声明二进制的字节身份，不声明来自当前源码。

历史原始数据、`.data/`、公开 CSV/JSON 与来源摘要保持原样。保留的 Python
`archive.py`、`docs.py`、`update.py`、绘图器和历史审计器只处理展示/归档证据，
不属于活动测量链路。Python dispatcher、计时 CLI 和重复准备器不保留兼容壳；
历史复现使用原批次冻结 harness，
不能用当前代码冒充旧时钟。

```sh
python3 benchmark/docs.py check
```

站点直接构建普通 Markdown 表格，不加载自定义表格 renderer，也不编译测试工具。
六处原动态表格已内联，`docs.py check` 仍核对 JSON、CSV 与文章中的精确表格。
更新经过审核的数据时必须同步更新双语 Markdown；修改 JSON 不会自动改写文章。
原公开 binding 不接受新的 Criterion cohort；新测量发布仍须独立登记匹配的口径、
完整证据和 REGISTRY 准入。迁移本身不更新任何用户数字、不合并历史样本。

## 验证

```sh
mise run test -- pvisor-benchmarks -- --manifest-path benchmark/criterion/Cargo.toml
mise run benchmark -- --smoke --criterion-output benchmark/.data/harness-smoke
mise run test-benchmark-tools
mise exec -- cargo check --locked --manifest-path benchmark/criterion/Cargo.toml \
  --features vm-probes --bin pvisor-memory-vm --bin dax-driver --bin startup-stage-mount
```

`--smoke` 是工具自测：故意在测量区间外等待，验证自报时间以及 B/operation 的归一化；
不是产品性能结果。默认测试不运行昂贵 VM/镜像/内存 campaign。编译、fixture smoke、
真实后端单次验证与正式实验必须分别报告，尤其不能将 Linux 单元测试算作 macOS 或
KVM/stock-kernel/Docker 环境验证。
