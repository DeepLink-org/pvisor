# 真实 KVM guest 的等待可见性与 observer 成本

Benchmark: **B-VCPU-IDLE-ENG**，角色 **engineering A/B**，EXP-001 **M0 observe-only**。数据日期：2026-10-07。只描述 Linux x86_64/KVM 的 GNU debug binary，不进入用户 benchmark 正文。

## 主要结论

**真实 VM API 已连通、校验通过，但 KVM M0 不能从这些负载识别精确等待窗口：正式批 70/70 有效、0 失败、0 timeout，16,666 条 vCPU 状态中 99.946% 为 Unknown，等待窗口为 0。** 7 个 case 的配对 wall 差异 95% CI 均包含 0，因此未检出整任务 wall 差异。observer on 的 VM CPU 时间配对差异中位数为 +48.253～+88.676 ms；6 个 case 的 CI 为正，2-vCPU busy 的 CI 包含 0。

这说明 on 路径的观察/记录并非免费，但不能将 KVM 的 Unknown 当作 guest 忙或闲。sleep、短 timer 未命中窗口不证明 guest 没有等待；busy/SMP 未误报窗口也不能替代 HVF 聚合或自动策略验收。**卸载收益未测，M1/M2 未实现，没有自动 offload。**

## Motivation

决定当前后端能否解释真实 guest 等待机会，以及是否值得进一步研究 observer 与未来策略。真实 sleep、短 timer 与单核 busy SMP 必须分别观察，不能用低 CPU 或 KVM_RUN 没有返回推导全 VM idle。

## 实验设计

- 同一宿主 Fedora/Linux `7.2.8-200.fc44.x86_64`，host affinity 0–15；没有 host pinning 或 cgroup quota，guest worker 分别 pin 到 CPU 0/1。
- GNU debug example，rustc `1.98.1`，`x86_64-unknown-linux-gnu`；实际动态加载 `libkrunfw.so.5`，没有 embedded kernel。每格真实 fresh VM、256 MiB、1 或 2 vCPU，最多一台 VM 同时运行。
- 7 cases：sleep/busy/short-timer × 1/2 vCPU，另加 2-vCPU smp-one-busy。每 worker 3 秒，短 timer 为 2 ms sleep；默认 10 ms sample interval，VM 外部 timeout 90 秒，线程 deadline/cap 保持原协议。
- 每个 case 5 个随机 off/on pair，共 35 pairs / 70 VMs；seed `20261007`，启动前保存完整 schedule。正式批与独立 pairs=1 预检、旧失败批完全分开。
- `Queue` 和 `Process` 都来自 `mp.get_context('fork')`。Linux guest 父进程在启动 worker 前没有线程，不依赖 Python 3.14 的默认 forkserver。done 记录实际 `start_method`，runner 校验为 fork；没有放宽任何已有正确性/时间/拓扑校验。
- 每个 worker 的完整 64 KiB payload SHA-256、CPU affinity、iterations、CPU 时间与单调起止均保留。worker 工作区间必须重叠至少 1.5 秒，正常 exit=0、guest 成功标记、观察者完成记录及真实 VM 退出全部通过才有效。
- ready callback 的真实 `VmmHandle` → `set_vcpu_observation(bool)` → `vcpu_observation()`，校验 session、topology、sequence、计数、聚合、拒绝原因与样本上限；Linux ready maps 必须包含所选动态固件。
- `wall_s` 是 VM 启动到 reap，包含启动/握手/任务/退出，rootfs 复制与资产清单在 timer 外。`cpu_s` 来自 VM 子进程 wait4，包含所有 VMM/采样线程；不包含组外 Python 协调者与复制成本。
- on/off 差异包含 collector + sampler + JSONL 编码/写入，不隔离纯 collector 成本。off 使用同样 guest 与握手轮询，但不周期采样。
- 单测/构建先完成；pairs=1 完整通过后才单独运行 pairs=5，执行期间本 agent 未并行构建或测试。没有改变 global sysctl/sysfs。没有自动宿主干扰监控或事后剔除慢样本，不能排除未观测的背景干扰。

## 实验数据和分析

### 正式批：开销

全部来自 `.data/vcpu-m0-fork-pairs5-20261007/report.json`，**每行 n=5 有效 pairs，invalid pairs=0**。单位均为 **ms**；差异为 pair 内 on-minus-off 后取中位数，CI 为 2000 次 paired bootstrap 的 95% CI，未跨负载或 cohort 合并。

| 负载 | vCPU | Δ wall 中位数 [95% CI] | Δ VM CPU 中位数 [95% CI] |
|---|---:|---:|---:|
| sleep | 1 | +0.099 [-30.302, +63.932] | +48.253 [+44.236, +131.273] |
| sleep | 2 | +16.646 [-26.791, +53.506] | +88.676 [+62.841, +161.924] |
| busy | 1 | +9.897 [-49.659, +50.738] | +52.526 [+6.286, +74.269] |
| busy | 2 | -0.035 [-91.588, +41.068] | +55.581 [-29.322, +94.595] |
| short-timer | 1 | -7.043 [-20.205, +40.388] | +52.174 [+24.102, +129.585] |
| short-timer | 2 | -0.108 [-30.202, +20.035] | +66.418 [+22.800, +70.510] |
| smp-one-busy | 2 | +20.108 [-17.746, +32.148] | +72.692 [+56.787, +112.784] |

所有 wall CI 含 0；2-vCPU busy 的 CPU CI 也含 0，均标为“未检出差异”，不称略快/略慢。n=5 的 bootstrap 区间只是这个小批次的工程估计，不是生产 QoS、尾延迟保证或多 case 校正后的显著性结论。

基准水平如下，每个 off/on 条件 n=5，均为中位数：

| 负载 | vCPU | wall off / on (s) | VM CPU off / on (ms) |
|---|---:|---:|---:|
| sleep | 1 | 3.421849 / 3.418229 | 396.468 / 459.047 |
| sleep | 2 | 3.417018 / 3.435822 | 532.072 / 632.044 |
| busy | 1 | 3.402166 / 3.401233 | 3364.123 / 3416.649 |
| busy | 2 | 3.410037 / 3.413153 | 6498.495 / 6555.384 |
| short-timer | 1 | 3.413336 / 3.406216 | 488.410 / 543.985 |
| short-timer | 2 | 3.413569 / 3.414117 | 738.664 / 802.226 |
| smp-one-busy | 2 | 3.403353 / 3.412549 | 3505.878 / 3575.076 |

**配对差异的中位数不是 on/off 两组中位数之差。** 审查了逐条件 5 个 wall 读数；样本过少，不划分统计簇、不推断总体单峰/双峰，也不报告 P95/P99。3 秒的固定时间负载主要检验观察者成本，不能仅凭 wall 不变证明工作吞吐不受影响。

### 正式批：窗口、Unknown 与采样

下表只统计 35 个 on trials 的周期采样，不包含 off 的 disabled 初始 snapshot。`snapshot` 每次包含全部配置 CPU，因而 vCPU 状态记录数和 snapshot 数的分母不同。

| 负载 | vCPU | snapshots | vCPU 状态记录 | Unknown | Unknown 比例 | sample-call-and-encode 中位数 (µs) |
|---|---:|---:|---:|---:|---:|---:|
| sleep | 1 | 1513 | 1513 | 1508 | 99.670% | 23.764 |
| sleep | 2 | 1509 | 3018 | 3018 | 100.000% | 28.884 |
| busy | 1 | 1525 | 1525 | 1522 | 99.803% | 17.653 |
| busy | 2 | 1515 | 3030 | 3030 | 100.000% | 22.622 |
| short-timer | 1 | 1522 | 1522 | 1522 | 100.000% | 23.714 |
| short-timer | 2 | 1513 | 3026 | 3025 | 99.967% | 29.014 |
| smp-one-busy | 2 | 1516 | 3032 | 3032 | 100.000% | 25.132 |
| **合计** | — | **10613** | **16666** | **16657** | **99.946%** | **24.566** |

其余 **9 条状态均为 HandlingExit**，Executing=0，WaitingForEvent=0。snapshot 拒绝原因为 Unknown **10605/10613（99.925%）**，NotAllWaiting **8/10613**；没有 WakeDeadlineUnavailable。所有 case 的 sampled wait epochs、collector idle_epoch、completed/active all-waiting time 均为 **0**。每个 on trial 远低于 **6302** 个样本上限。

合并 sample-call-and-encode 时间的描述性中位数 **24.566 µs**，最大 **208.619 µs**；最大值不是 P99。这个计时只含 API snapshot + JSON Value 构造，不含 JSONL 写入，也不能解释为纯 collector 接点/锁开销。它没有替代上面的整 VM CPU A/B。

busy worker 的 SHA-256 iterations off/on 描述性中位数为：1-vCPU busy **121372 / 121341**（每条件 5 workers），2-vCPU busy **121408 / 121450**（每条件 10 workers，两个 worker 嵌套在同一个 VM，不能当独立样本推断），smp-one-busy **121224 / 121266**（每条件 5 busy workers）。这些 worker 的 CPU 时间中位数都约 2999 ms，支持负对照确实计算了约 3 秒；不据这些微小数值差异宣称吞吐收益。

### 完整性与历史失败

| 独立 cohort | pair/case | 有效 trials | 失败 trials | complete | 用途 |
|---|---:|---:|---:|---|---|
| `.data/vcpu-m0-preflight-20261007` | 1 | 0/14 | 14 | false | 原始失败保留，不用于收益统计 |
| `.data/vcpu-m0-fork-preflight-20261007` | 1 | 14/14 | 0 | true | 独立修复预检，不合入正式批 |
| `.data/vcpu-m0-fork-pairs5-20261007` | 5 | 70/70 | 0 | true | 正式工程 A/B |

原始 `t0000/stderr.log` 记录 Python 3.14 forkserver 重导入 `/vcpu-work.py` 时的 multiprocessing bootstrap RuntimeError。原始目录未覆盖、未删除；14 个 runner 终态均为 VM 非零退出/timeout 类失败，不能当 0 耗时。新的回归测试强制宿主 Python 全局默认 forkserver，执行实际嵌入 guest 脚本（临时目录及 host affinity 映射），显式 fork 仍成功；另测错误 start_method 被 runner 拒绝。**16 个单测通过**，debug build、rustfmt check、Python compile 通过。

新 preflight on 条件保存 **2123 snapshots / 3333 vCPU 状态**，Unknown **3331（99.940%）**、HandlingExit **2**，WaitingForEvent=0；不将这些计数加到正式批。

### 测后统计一致性修复

实测后只读复核发现：snapshot 时间戳在 collector 锁外采集，以及 disable 未截断逐 CPU 的等待记账。当前源码已修复这两点并新增回归测试，`just test pvisor-vm` 为 345 passed、8 skipped；冻结 binary/receipt 未替换。本报告的 70 次实验仅验证冻结的修复前版本，不是修复后源码的运行验收。KVM 实验没有 WaitingForEvent，不能据此证明 HVF 等待统计；当前源码再次测量必须重新构建并生成独立 cohort。

### 真实限制

- KVM M0 在 KVM_RUN 内保守 Unknown。这里已经验证真实 API 采样与负载正确性，但不能识别 guest 等待，更不能授权自动卸载。
- sleep 和短 timer 都是 Python 合成负载，不是 Agent、网络阻塞或 futex 的代表性生产集合。HVF、host 拥塞、timer 取消/重编程、wake latch、跨 CPU wake、卸载/恢复均未测。
- 采样范围包含 worker 创建与 done 前的收尾，guest/host 时钟没有转换合同；不从某条 host 状态推导某个 guest CPU 的精确稳态语义。10 ms 采样可能漏短接点，不能把未采到当未发生。
- 构建为 debug，只有一个宿主、每 case 5 pairs，未施加 host cgroup 限额或做连续干扰监测。没有全机 CPU/PSS、memory-time integral、生产密度或 release 成本结论。
- 失败和慢有效样本都保留；本批 invalid pairs=0，没有挑样本。来源、rootfs、firmware、init 在批末复核通过，但 source/binary receipt 不等价于完整可重现 toolchain/sysroot/动态依赖的证明。
- offload 没有调用，卸载收益未测；M1 wake/deadline、M2 自动 offload 均未实现。

### 来源与身份

新预检与正式批使用相同 binary、source manifest 和输入，统计仍保持独立。Git HEAD `2a9e38b34d2ddc5055bfbceca6e489c70834c937`；dirty patch 与冻结源码保存在各 cohort 的 `build/`。下列均为 **SHA-256**：

| 制品 | SHA-256 |
|---|---|
| 新 binary | `af6830774dfd9fef042893c7491c38077377cf7032afeb1add3af433ca83f239` |
| 新 `sources.json` | `598627d704cbfcc5c0182bd7cc8300053c49c22e065a095e959d5c36b457ce73` |
| example source | `ebe66c9189db4fe0b00fc78c8e11a8e1961b2e53d0ec1a0f0dd1e86bf6f023c7` |
| runner source | `9271f039062e4ee43377b89b562e0378b695725676d4f908ac9265f8e4c72d71` |
| regression tests | `1b08c28ed9134265ffa9eb35bb7102129157dd099b94c98cab7500326364aa8e` |
| **未改动**的 plan | `baf518ba8c366aef9cd4e3267cacab88d27a4154747f2675e99ddd831387cee1` |
| firmware `libkrunfw.so.5` 的目标文件 | `b61f68dac3ef20a88e1ee387733e4baed2f7c02edac940c8882e0b549dac95e4` |
| static pvisor-guest init | `0d3ddd020ed5943d294c4f3ad981b5e9373348d746dc7921878972bca8e514e2` |
| 完整 `inputs.json`（三批一致） | `afd9f8c4e32505aed286820587caf64993c88a1107db9df5ed196afe1cc87fd6` |
| rootfs 子清单 canonical JSON¹ | `88f30a875d3997c592f366d15c6d8719eff28572a5e9596b4264ff7950d5b2f1` |
| 原始失败 `report.json` | `b080ffae74ac442762836236dada72d53359099efa459d2e8d5f5e32532ba975` |
| 新预检 `report.json` | `0767a4576627b52d1bf3d887231c96f0e16da98cc372af9c2fd9d772fe89951e` |
| 正式 `report.json` | `036094ed87fcc5ffd7d7c73d28cd932350bbd16499fae6b8ae28f4655e5bf339` |

¹ `sha256(json.dumps(inputs['rootfs'], sort_keys=True, separators=(',', ':')).encode())`；这是含内容/模式/符号链接的清单摘要，不是 rootfs archive 字节摘要。payload 校验值为 `7daca2095d0438260fa849183dfc67faa459fdf4936e1bc91eec6b281b27e4c2`。

source inventory 现在精确登记 runner、plan 和 regression tests，排除派生的 `vcpu_idle_report.md`，因此加工报告不会使收据失效；没有修改 plan。两批 runner 的批末 current/frozen 来源门禁及正式批结束后的首次 `verify_build` 均通过。

**测后工作树漂移：**报告加工期间，外部将 `crates/pvisor-vm/src/api.rs` 的 `shared_mapping` 注释新增两行（关于 pool RAM 不支持跨 fork 继承/转移）。该文件由冻结值 `c0cb9f442966d30bd962473f5898c1e70cf2ed9092855b6e1764d8107560ecc7` 变为当前值 `73bf4dcbda7b8c83da4401f503bd22bcfa32928748d0cc0a9860b688b8ea50a6`。它不是本实验的代码修改，没有回滚；即使仅为注释变化，当前工作树再次 `verify_build` 仍按规则拒绝。实验报告、冻结 source/binary/SDK 及全部输出另行复核通过，结果保存在两个新 cohort 的 `post-run-audit.json`；最大 samples/trial 均为 305。数字绑定冻结源码，不绑定此后最新工作树。未来再次测量应生成 NEW build receipt，不复写本批 receipt。

### 实际命令

从仓库根运行。测试/构建执行完后，才单独运行预检与正式批；终端每次 timeout 均不超过 600000 ms。以下目录为已经保留的证据；重新执行必须选 **NEW** 目录，不能覆盖它们。

```sh
rustfmt --edition 2024 crates/pvisor/examples/vm_vcpu_observe.rs
python3 -m unittest discover -s benchmark/pvisor -p test_vcpu_idle.py -v
rustfmt --edition 2024 --check crates/pvisor/examples/vm_vcpu_observe.rs
python3 -m py_compile benchmark/pvisor/vcpu_idle.py benchmark/pvisor/test_vcpu_idle.py
CARGO_BUILD_JOBS=4 python3 benchmark/pvisor/vcpu_idle.py --build \
  --output benchmark/pvisor/.data/vcpu-m0-fork-build-20261007

python3 benchmark/pvisor/vcpu_idle.py \
  --build-receipt benchmark/pvisor/.data/vcpu-m0-fork-build-20261007/build-receipt.json \
  --rootfs /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/density-env4/rootfs \
  --firmware /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/firmware \
  --init /home/reiase/workspace/pvisor/target/pvisor-guest/x86_64-unknown-linux-musl/release/pvisor-guest \
  --python /usr/bin/python3 \
  --output benchmark/pvisor/.data/vcpu-m0-fork-preflight-20261007 \
  --pairs 1 --seed 20261007 --seconds 3 --interval-ms 10 --timeout 90

# 仅在上述预检 complete=true、14/14 valid 后执行，独立 NEW cohort。
python3 benchmark/pvisor/vcpu_idle.py \
  --build-receipt benchmark/pvisor/.data/vcpu-m0-fork-build-20261007/build-receipt.json \
  --rootfs /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/density-env4/rootfs \
  --firmware /home/reiase/workspace/pvisor/benchmark/.data/full-retest-20261006/firmware \
  --init /home/reiase/workspace/pvisor/target/pvisor-guest/x86_64-unknown-linux-musl/release/pvisor-guest \
  --python /usr/bin/python3 \
  --output benchmark/pvisor/.data/vcpu-m0-fork-pairs5-20261007 \
  --pairs 5 --seed 20261007 --seconds 3 --interval-ms 10 --timeout 90
```

原始 stdout/stderr、逐次命令、guest/observer 输出、全部 JSONL、输入清单、source/binary receipt 与失败留在 `.data/`，没有发布为站点下载。复现约束见 [runner README](README.md#vcpu-observation-m0)；本报告的所有开销数字仅加工自上述正式 report 的 35 个完整 pairs。
