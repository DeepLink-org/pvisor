# macOS/HVF 文件系统优化复测

## Motivation

Apple Silicon 上这些优化的收益不能照搬 Linux/KVM 结果。本次七项独立
开关 A/B 中，六项主要指标检出改善，一项未检出；prepared-copy-up 的
单独可写 OPEN 明确变慢，但与并发 metadata 全部完成的时间改善。
所有实验开关仍默认关闭，不据此统一默认开启。

范围为 `1a4a8dfb..93f32740`（`3a8b07cd` 与 `93f32740` 两次提交）。
本次只改实验脚本的平台适配、离线合同测试和报告，没有修改产品实现。
同 release 二进制 off/on 的实验用于识别机制收益与局部退化；
没有测优化前后默认配置的版本级性能，也没有完整 Agent 性能结论。

## Experiment design

注册条目：B-FS-ENG。Apple M4、24 GiB RAM、macOS 27.0.1 arm64，
Darwin 27.0.0，Rust 1.98.0；实际 HVF guest，不是模拟或单测计时。
SDK driver 和 CLI 均为当前提交 release 构建，使用 Hypervisor entitlement
签名后冻结；guest 用 `aarch64-unknown-linux-musl`、`opt-level=2` 构建。
2vCPU/512MiB guest。宿主不设 affinity、CPU quota、内存或 swap 限制，
不清宿主页缓存、不设干扰剔除；未采集温度/功率与系统负载时间序列。
这些条件与此前 Linux 受限批次不同，不作跨平台速度排名。

每项 N30 seeded 随机配对，新 VM/容器内三轮累计为一个环境样本；
沿用原 sampler 的输入、正确性门禁和计时边界。SDK 每项另有 N5
stat256/scan300/open10000 控制；每项 off/on 各一份独立 profile，
核对注册和唯一 final、实际资格及机制计数，不把插桩时间计入正式样本。
七批共 450 个计时环境、14 个诊断环境，全部 valid、failures=0、
excluded=0。每项 5,000 次配对 bootstrap，逐项 95% 区间，未作多重比较校正。
正降幅表示更快，由配对 on/off 比值计算，不是两个中位数之比。

SDK 场景使用明确 Immutable lower，copy-up 从外层校验全部 lower/upper
字节后才清理自有夹具。CLI 场景使用本地 `FROM scratch/COPY` V2 镜像，
`run --rm --network none`，无 registry 下载；guest 核对完整内容。
逻辑目录实验只切换自身，名称/prepared-copy-up 固定开启；
lazy-nodes/read-sink/direct-copy 固定逻辑目录开启，只切换该后续机制。
不把各项百分比累加。

`logical-data` 的实现受 Linux data-object 协议限制，在 macOS 不启用，
本次没有把 no-op 开关作为性能实验。对象级 READ 屏障没有真实混合 I/O
计时，仍未验收。额外在四种 CLI 负载上将逻辑目录、lazy 节点复用、
流式读、直接复制同时开启（名称和 prepared-copy-up 也开启），全部
通过正常退出和 guest PASS 校验；这四次仅为组合功能 smoke，不作计时结论，
也不能替代并发、内存压力或完整 Agent 验收。

### 制品、复现与失败保留

固件为构建缓存中的 `libkrunfw.5.dylib`，SHA-256：
`83a2cc4a8f5817e13ee74cbbd14d78ddbae3fda28b2f24355a7e4a62f86497d1`。
SOURCE 收据中的 patches、Makefile 和 aarch64 config 与当前源码匹配。
最初使用的 `target/release` 固件副本摘要与收据不同，两个 `*-01`
准备目录被拒绝并保留，没有绕过摘要门禁；正式批次改用收据匹配的缓存制品。

正式批次均位于本地 `benchmark/.data/`，不随仓库分发：

| 实验 | 有效批次 |
| --- | --- |
| directory-names | `macos-names-02` |
| prepared-copy-up | `macos-prepared-copy-up-01` |
| scoped-names | `macos-scoped-names-01` |
| logical-directory | `macos-logical-directory-03` |
| lazy-nodes | `macos-lazy-nodes-03` |
| read-sink | `macos-read-sink-03` |
| direct-copy | `macos-direct-copy-03` |

各批冻结源码、脚本和制品，保留 manifest、逐次 argv、stdout/profile、
records、status 和 analysis。`macos-logical-02` 被外层终端 200 秒总期限
中断，已标 invalid；保留其 57 条记录，不从中抽取结果或补样。
后续正式批次从头运行且未设这个外层期限，单环境仍保留 90 秒超时。

复现时先构建并签名：

```sh
mise exec -- cargo build --locked --release -p pvisor-vm --example guest_metadata_case
mise exec -- cargo build --locked --release -p pvisor-cli
mise exec -- rustc --edition 2024 --target aarch64-unknown-linux-musl \
  -C linker=rust-lld -C opt-level=2 \
  crates/pvisor-vm/src/probes/guest_metadata_guest.rs -o target/metadata-guest
codesign --force --sign - --entitlements crates/pvisor/macos-hypervisor.entitlements \
  target/release/examples/guest_metadata_case
codesign --force --sign - --entitlements crates/pvisor/macos-hypervisor.entitlements \
  target/release/pvisor
```

依照本目录 README 的 `--prepare` 命令传入这些制品及匹配的 FW/SOURCE，
选择新批次目录、`--samples 30` 与对应 `--experiment`，然后执行：

```sh
target/semspec/debug/semspec bench --cwd "$BATCH" "$BATCH/in.json"
python3 benchmark/filesystem/directory_names_analysis.py "$BATCH"
```

`BATCH` 使用绝对路径；先按 `benchmark/README.md` 构建 semspec。
macOS manifest 显式记录 `host_platform=darwin`、`host_budget=unrestricted`
及空 cpus，不宣称 Linux cgroup 限制。长临时目录容易触发 macOS
Unix socket 路径上限；常规 Rust 测试用 `TMPDIR=/tmp`，实验夹具仍由
各批次自己的 work 目录管理。原始制品副本不再重新签名或更改摘要。

## Data and analysis

### 七项主要指标

单位为每环境三轮累计 ms。

| 优化 / 主要指标 | off 中位 ms | on 中位 ms | 配对耗时降幅 | 95% 区间 |
| --- | ---: | ---: | ---: | --- |
| 名称快照 / 8192 项目录枚举 | 666.089 | 641.289 | 3.34% | [2.95%, 4.42%] |
| prepared-copy-up / OPEN 与 metadata 全完成 | 103.397 | 73.478 | 26.89% | [23.57%, 30.11%] |
| 范围失效 / 名称 reimport | 592.988 | 593.062 | 0.02% | [-0.48%, 0.54%] |
| 逻辑目录 / 2048 文件 stat | 1441.864 | 1303.227 | 9.32% | [8.79%, 9.84%] |
| lazy 节点复用 / 深路径 stat | 1541.539 | 1483.725 | 3.76% | [3.44%, 4.19%] |
| 流式读 / 重开大文件读 | 25.435 | 23.876 | 6.53% | [5.29%, 7.42%] |
| 直接填充 upper / copy+文件及父目录同步 | 135.900 | 66.198 | 50.79% | [49.99%, 52.61%] |

### 局部退化、次要指标与控制

**prepared-copy-up 的可写 OPEN 明确退化。** OPEN 从 67.577 到 70.853ms，
配对降幅 -6.83%，区间 [-9.22%, -3.24%]；metadata 从 103.224 到
42.741ms，降幅 61.82%，[52.04%, 68.90%]。总完成更快不等于复制
OPEN 本身更快。只重视单次可写打开、没有并发 metadata 的用户负载
尚未独立测试，不能由此总指标推荐默认开启。

其他次要指标：逻辑目录读回降幅 11.54%，[4.38%, 19.42%]；
lazy 节点复用读回 10.55%，[9.85%, 10.85%]；
流式读首次读 1.43%，[0.36%, 3.30%]；
直接填充 OPEN 77.68%，[74.12%, 78.05%]。
同步后的完整收益为 50.79%，不能只报 OPEN 收益。
范围失效的整个编辑循环降幅 -0.34%，[-1.19%, 0.96%]，仍未检出。

三项 SDK 实验的 N5 stat/scan/open 控制均未检出差异，部分点估计为负；
没有删去慢样本。N5 控制不证明严格等效，N30 也不足以报告 P95/P99。
名称范围失效继续保留为无收益实验，不建议默认启用。

### 功能回归与验收边界

最终执行：

```sh
TMPDIR=/tmp mise run test -- pvisor-vm pvisor-overlay-core pvisor -- \
  --lib --tests --no-fail-fast
```

1326 项运行，1324 通过、2 失败，另 20 跳过，**测试命令没有全绿**。
两个失败在优化前 `1a4a8dfb` 的源码归档中用同样 runner 定向复测，
均以相同断言失败：

- `environment_snapshot_tree::owned_copies_isolate_source_and_sibling_writes`：
  复制后 mode 为 2541（0o4755），期待 3565（0o6755）。
- `environment_snapshot_store::layer_sharing_checks_reference_volume_and_private_copy_remains_available`：
  materialize 返回 `filesystem inventory mismatch`。

因此这两项不是本 patch 新增回归，但本机 snapshot 权限/库存问题仍存在，
本次不修改无关实现。最初长 TMPDIR 测试的 socket 长度失败、一次 HVF
创建失败在短路径最终复测中消失；缺 debug CLI 的 Seatbelt 失败在构建
CLI 后消失；一次 PTY 失败定向和最终重跑均通过，保留为间歇失败记录。

`mise run test-py -- benchmark/filesystem/test_directory_names_ab.py
benchmark/filesystem/test_logical_directory_ab.py`：9 通过；
四个变更 Python 文件 Ruff check、format --check 及 git diff --check 通过。
release host probe、debug/release CLI 构建通过；构建仍有现存 macOS
unused-variable/dead-code 告警，不声称严格 Clippy 全绿。
本次未运行 workspace 全量测试、完整 Python 套件、Linux 平台复测、
内存压力/多 VM 密度、对象级 READ 屏障、完整 Agent 或默认配置版本级 A/B。
本地常规测试/构建日志与四次组合 smoke 位于
`target/macos-fs-validation/`；基线源码与构建也留在那里。
