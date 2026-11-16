# 网络场景与编译优化对照

## Rust/Criterion active entry

`profile_ab.json` is now a Criterion locator for
`benchmark/criterion/src/network/mod.rs::register`. The Python commands below
describe the historical protocol and its read-only audit, not a new measurement
entry. The active runner starts the existing integrity-checked Rust `worker.rs`
directly for the loopback origin, native client and guest; it never shells out
to a Python measurement adapter.

The parent standalone crate owns Criterion 0.7.0 wiring. Direct execution uses:

```sh
export PVISOR_BENCH_FAMILY=network
export PVISOR_BENCH_ENTRY=network/profile_ab.json
export PVISOR_BENCH_OUTPUT=/absolute/benchmark/network/.data/NEW_BATCH
export PVISOR_BENCH_BASELINE=/frozen/release-pvisor
export PVISOR_BENCH_CANDIDATE=/frozen/net-o3-pvisor
export PVISOR_BENCH_NET_WORKER=/frozen/static-net-worker
export PVISOR_BENCH_FIRMWARE=/frozen/firmware-directory
export PVISOR_BENCH_CPUS=0,1,2,3
export PVISOR_BENCH_SERVER_CPUS=4,5
```

Optional `PVISOR_BENCH_BUILD_RECEIPT` supplies separately audited build records;
digests alone do not prove same-source/package-profile provenance. The worker
must be built once, statically, for the guest and host. The runner freezes both
CLI artifacts, worker, firmware and the supplied receipt, checks exclusive
loopback resolution, and starts a bounded-readiness private origin. It requires
Linux x86_64/KVM. Four client CPUs and two server CPUs must be disjoint.
Build first, then execute the frozen runner directly: do not measure under
`cargo run`/`cargo bench`, whose still-running Cargo parent is correctly
identified as build interference.

Three checked randomized warmup cohorts precede Criterion. A real Criterion
operation randomizes the three arms and scenario order, gives every VM arm a
new private rootfs/workspace/Session, and retains each worker's internal clocks,
per-request samples and exact byte/flow/DNS policy accounting. The entire
process group has a 150-second deadline; network connections retain the Rust
worker's 15-second I/O deadlines. Origin and CLI process groups are always
reaped. External cargo/rustc interference invalidates the batch rather than
killing someone else's build. Slow valid samples remain in the evidence.

Criterion owns a new >=30-sample cohort; its calibration invocations
also receive full checks and raw evidence. These are not historical fixed
3/30 Python samples. Raw worker clocks, not the whole paired operation time,
define the measured network intervals. `PVISOR_BENCH_METRIC` selects a scenario
(default `rpc_fresh`) or `stream_first_byte`. Separate `release`/`net_o3`
benchmarks each still execute the full random three-arm cohort but submit only
that arm's selected **complete request-batch** interval, named ns per batch.
They do not sum or normalize per-request medians. Raw per-request clocks and
bulk byte counts remain available for distribution and goodput interpretation.
Criterion owns all statistical estimation. Its independent arm intervals are
not paired-bootstrap contrast intervals; there is no automatic paired
inferential publication. Complete randomized raw pairs are retained for review.
Do not combine scenarios from one VM as independent samples, derive request
P95 from Criterion estimates or publish the outer cohort wall time as latency.

Offline tests cover result inventories; existing standalone worker tests still cover duplex payload
checking and upload receipts. A compile/test pass does not establish KVM,
firmware, endpoint policy or same-source build-profile equivalence. No
historical report, publication binding or user-network table is rewritten.

问题：网络热路径从体积优化改成速度优化后，真实 VM 的哪些网络场景有改善？
本入口属于 [B-NETWORK-PROFILE-ENG](../REGISTRY.md#b-network-profile-eng)，
不填充仍未准备的用户网络表，不改历史数据。

## 对照制品

使用相同源码、锁文件、features、target 和固件构建两份 CLI。先冻结基线，
再构建候选。当前默认 release 已采用网络 O3；复现体积优化基线须显式覆盖：

```sh
--config 'profile.release.package.pvisor-overlaynet.opt-level="z"' \
--config 'profile.release.package.smoltcp.opt-level="z"'
```

候选使用默认 release；旧批次在默认值改变之前通过以下等价覆盖构建候选：

```sh
--config 'profile.release.package.pvisor-overlaynet.opt-level=3' \
--config 'profile.release.package.smoltcp.opt-level=3'
```

不能把整个 `--profile performance` 制品当成上述局部变化。构建命令、源码摘要、
Cargo 输出和最终二进制 SHA-256 必须留在批次中。静态 worker 用同一份
`rustc --edition=2021 --target x86_64-unknown-linux-musl -C linker=rust-lld -C opt-level=3`
产物，在 guest 与原生条件复用。guest supervisor、固件和网卡能力保持不变。

## 场景和统计

每个样本启动新 VM，运行一次完整场景集合，内部网络时钟不包含启动：

- 128 次纯 TCP connect/close。
- 128 次新连接的 1 KiB RPC。
- 一条长连接上的 256 次 1 KiB RPC。
- 128 次合成 DNS 查询（仅 VM；不是公网 DNS）。
- 32 个 64 B 事件、服务端间隔 2 ms 的流式读取。
- 64 MiB 单连接上传与下载。
- 8、32 连接上传与下载，每场景总计 64 MiB。

RPC 与纯建连保留逐请求耗时；统计单位仍为每个新 VM 的批次中位数。
bulk 报告全连接完成的 wall goodput，而非平均各连接速度。
所有数据逐字节验证；上传完成时钟包含服务端校验回执。
stream 的首字节包含建连和协议头，完整时间包含服务端的预定等待。
线程启动与建连发生在 bulk 时钟之前；数据生成、传输和完整性验证包含在时钟内，
其成本在三个条件相同，不能解释为纯 VMM 带宽上限。

每轮条件和场景随机交替，3 次预热与至少 30 个独立样本；配对 bootstrap 95% CI。
固定 CPU 亲和性而非 CPU 硬配额，2 vCPU/256 MiB 不代表整机内存上限。
不同场景共享一台样本 VM，不能将它们合并成更多独立样本。
正式采样不可与 cargo/rustc 构建重叠，外部构建使批次无效；其他宿主负载保留观察。
慢样本不删除，失败保留并使批次无效。

## 执行与证据

```sh
python3 benchmark/network/profile_ab.py \
  --baseline /absolute/frozen/release-pvisor \
  --candidate /absolute/frozen/net-o3-pvisor \
  --worker /absolute/frozen/net-worker \
  --firmware /absolute/libkrunfw-directory \
  --build-receipt /absolute/build-command-and-cargo-logs \
  --output benchmark/network/.data/NEW_BATCH \
  --cpus 0,1,2,3 --server-cpus 4,5
```

输出目录必须不存在。服务只绑定 loopback；预检要求
`localhost.localdomain` 的宿主解析结果全部为 loopback。guest 使用公共
OverlayNet 合成 DNS 与正常策略授权出口，不添加绕过数据面的接口。
每次运行使用新的私有 rootfs、Session 状态与 workspace。
不更改全局配置、不拉取镜像、不访问公网、不清理宿主缓存。
原始 JSON、命令/stdout/stderr、主机观察、冻结代码与制品摘要放 `.data/`；
成功后生成 `report.md` 和 `summary.json`。外部制品无构建收据时只记录其摘要，
不能声称已证明它来自当前源码。

验证 Python harness：

```sh
mise run test-py -- discover -s benchmark/network -p 'test_*.py'
```

这些离线合约也包含在默认 `mise run test-py` 中，无需 KVM、固件或网络服务。

worker 为独立、无 Cargo 依赖的双端网络负载；其特殊 runner 是
`rustc --edition=2021 --test benchmark/network/worker.rs -o WORKER_TESTS && WORKER_TESTS`。
它不是产品 Rust 测试的替代。网络 A/B 不覆盖 TLS、WAN、QUIC、peer 转发、
多 VM 密度或丢包恢复。

`profile_ab.json` 只登记独立配对协议，不是无参数的通用计时器入口；
`mise run benchmark -- network/profile_ab` 会要求使用上述显式制品命令。
原生客户端可使用四个亲和 CPU，VM guest 为两个 vCPU，宿主的 VMM/网络线程
也限制在四个亲和 CPU；原生行不是“相同应用 CPU 预算”的 VM 开销排名。

离线审计会使用批次冻结的 parser，重新检查全部预定命令、场景顺序、输出、
VM Bundle/精确字节计数、制品和固件摘要、Cargo features/profile 以及配对统计：

```sh
python3 benchmark/network/audit_profile.py benchmark/network/.data/EXISTING_BATCH
```

Cargo 的 package profile 覆盖也影响 smoltcp 的宿主 build script：旧批次为 `0`→`3`，
当前显式 `z` 基线的复测为 `z`→`3`；审计分别核对，不把它算成第三个运行时 crate 的变化。
当前工程结果见 [PROFILE_REPORT.md](PROFILE_REPORT.md)；默认产品 release 已采用网络 O3。
旧批次冻结的基线、构建记录和统计保持原字节，不用新的默认值重写历史证据。
