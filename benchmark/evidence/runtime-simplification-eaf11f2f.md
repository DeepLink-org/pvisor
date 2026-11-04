# 当前分支简化的性能复核

测量日期：2026-11-04。

## 结论

`eaf11f2f` 相对主干 `3b1f7821`，在本机所测范围内没有出现明显的完整任务性能回归，
也没有证据支持“删除大量代码带来了普遍加速”。启动存在小幅、方向不同的变化：
Host Ready 缩短约 2 ms，VM Ready 增加约 1.6 ms；两者的正常退出总时间均未检出差异。
staged 启动、Job 查询、七种 replay prepare-only 和扩大采样后的增量编译均未检出差异。
“未检出差异”不是等价性证明，也不代表所有路径没有回归。

对照问题：当前分支的整体简化是否改变文档所测的启动、文件密集任务、
Job 读取与轨迹准备成本？工程结果不覆盖原用户 benchmark，不将历史差值归因为本分支收益。

## 版本、环境和方法

- 基线：`3b1f7821f8b79e9a9988ca8b7e9ae9f1dbb36d86`。
- 候选：`eaf11f2f9baa6194cd6b218e9e7478b990cca8ad`。
- Ryzen 7 9700X，Fedora，Linux `7.2.11-300.fc45.x86_64`，Btrfs，CPU affinity `0,1`。
  宿主仍运行桌面程序，CPU governor 为 `powersave`；未清宿主页缓存、未修改全局调频、
  未停止用户服务。不同实验批次不合并。
- 两版本均从精确源码构建 release、musl、gateway 二进制；Rust 1.98.0、
  Zig 0.13.0。主干先通过 ambient cargo-zigbuild 0.20.1 构建，随后使用项目固定
  0.23.4 重建，确认 `pvisor` 和 `pvisor-replay` **逐字节相同**，因此已有测量制品
  与固定构建一致。构建日志和相等性证据随批次保留。
- 固件相同：Linux 6.12.109，固件 SHA-256
  `634d73f20c042894afa196fe396e561e9cf6b6a61c2e61fcd961eddb2e5cdf88`。
  静态 CLI 内嵌同一固件提取出的 kernel。
- 每个版本使用独立 user/mount/PID namespace、私有根、私有 `/tmp`、HOME 和
  Run store；启动前复制二进制，准备不计时。不连接或重启宿主 Host listener。
  Host/staged 实测隔离为 `rootless_process`，VM 为 `virtual_machine`。
- 启动复用 `startup/subjects.py`、`timing.py` 和 `hooks.py`，每模式先预检，
  3 对预热、60 对正式样本，seed `20261104` 随机交替版本；2 vCPU/128 MiB。
  每次同时记录首条精确 Ready 输出和正常零退出，校验 Bundle、隔离模式、
  stage 无文件写入和宿主 canary 未变。
- process/replay 先完整执行现有套件，再用其冻结的精确 argv、准备/校验 hooks 和
  `semspec/runtime.py` 做随机版本配对，seed `20261105`。
  process 使用 nightly 的 10 次预热/50 对；replay 每适配器零预热/60 对。
  后面的判断以配对批次为准，顺序批次仅保留作诊断。
- bootstrap 以完整配对为单位重采样 10,000 次，seed `20261106`，
  报告候选中位数减基线中位数的 percentile 95% 区间；区间包含 0 写“未检出差异”。
  区间未作多重比较校正，小幅显著差异应视为本批证据，不作为普遍保证。
  按 replay 文档的最大相邻间隔规则检查配对批次，未触发双峰判定。
- 无事后剔除；所有有效慢样本保留。正式批次任何正确性失败均使该批无效。

## 启动：文档历史水平与同机版本配对

单位 ms，中位数；历史数据来自 `docs/src/zh/benchmarks/startup.md` 的 2026-10-11 批次。
历史与当前的宿主内核、rootfs、二进制、固件细节和运行环境并非全部一致，**只并列水平，不算历史改善百分比**。

| Ready | 文档历史 N60 | 当前主干 N60 | 当前分支 N60 | 配对差值的 95% CI，ms |
| --- | ---: | ---: | ---: | --- |
| Host | 33.52 | 29.97 | 27.97 | [-5.160, -0.496] |
| staged | 50.59 | 40.59 | 40.52 | [-0.788, 0.490] |
| VM | 130.71 | 109.29 | 110.86 | [0.159, 2.703] |

本批 Host Ready 的点估计变化为 -6.66%，VM 为 +1.44%，staged 未检出差异。
这不表示完整命令也相应加速或减速：

| 启动至正常退出 | 主干 N60 | 分支 N60 | 配对差值的 95% CI，ms | 判断 |
| --- | ---: | ---: | --- | --- |
| Host | 41.19 | 41.36 | [-1.009, 1.368] | 未检出差异 |
| staged | 71.09 | 70.93 | [-0.305, 0.183] | 未检出差异 |
| VM | 172.29 | 172.59 | [-0.354, 0.797] | 未检出差异 |

新数据仍处于文档给出的启动量级。Docker、Firecracker 和 QEMU 没有重测，
不能拿本批候选值和历史竞品值重新排名。

## 文件系统：复测文档原工作负载

注册为 B-FS-ENG，使用 `filesystem/docs_rootfs_retest.py`，复用文档的静态 payload、
固定工具链、目录 rootfs 和独立 host 最终内容 oracle。M01–M12、S01–S03，
两版本 × 普通 VM/guest-writeback × 每格三个新环境，共 **180/180 有效试次**，
零剔除、零失败。每个环境执行三轮；统计三轮工作加最终同步之和，不将九轮当九个独立样本。
整个进程树 2 CPU quota/2 GiB/swap0；guest 2 vCPU/512 MiB。

下表为秒，N3 中位数 [最小–最大]。这是探索性水平，不推导稳定百分比。
文档历史数据来自 `docs/src/zh/benchmarks/filesystem.md`，同样单独列出。

| 任务/模式 | 文档历史 | 主干 | 分支 |
| --- | ---: | --- | --- |
| 仓库巡检 / VM | 2.376 | 2.311 [2.186–2.445] | 2.439 [2.343–2.450] |
| 仓库巡检 / writeback | 2.289 | 2.354 [2.332–2.435] | 2.344 [2.338–2.444] |
| 增量编译 / VM | 0.557 | 0.554 [0.551–0.568] | 0.600 [0.584–0.616] |
| 增量编译 / writeback | 0.353 | 0.381 [0.380–0.437] | 0.391 [0.377–0.392] |
| 解包发布 / VM | 11.675 | 12.022 [11.283–12.269] | 12.246 [12.001–12.522] |
| 解包发布 / writeback | 6.171 | 6.472 [5.791–6.490] | 6.397 [6.374–6.589] |

文档中“小写合并收益仍取决于同步语义”的判断保持成立：

| 三轮完成时间，ms，N3 中位数 | 文档 VM / WB | 主干 VM / WB | 分支 VM / WB |
| --- | --- | --- | --- |
| 1 KiB 写 | 649.800 / 31.084 | 685.056 / 33.062 | 676.668 / 32.762 |
| 1 MiB 写 | 24.794 / 31.827 | 25.518 / 34.245 | 25.795 / 32.132 |
| 逐 4 KiB fsync | 303.493 / 396.031 | 316.861 / 386.214 | 316.544 / 353.618 |

其余十二机制的完整水位、每轮数据、最终内容验证和预算记录保存在批次
`filesystem-full/analysis.json`、`records.json` 和各 trial 中；未合成总分。

### 增量编译疑点的独立 N30 复核

N3 普通 VM 编译的分支三次均高于主干三次，因此另开一个固定 S02、30 对环境的
确认批次；不从原批次挑样本补齐。配置和正确性 oracle 不变，
两版本 × 两配置 × 30 环境，共 **120/120 有效试次**。

| 三轮增量编译，ms | 主干 N30 | 分支 N30 | 配对差值的 95% CI，ms | 判断 |
| --- | ---: | ---: | --- | --- |
| VM | 572.04 | 579.57 | [-19.301, 40.411] | 未检出差异 |
| writeback | 375.34 | 380.43 | [-3.105, 11.825] | 未检出差异 |

原 N3 疑点没有在扩大采样后形成明确回归证据；区间仍允许数十毫秒的 VM 编译差异，
不应表述为已证明零成本。四个资源饱和点、Host/staged 和其他执行器未复测。

## Job 读取与 replay

随机配对的 B-PROCESS nightly，单位 ms：

| 命令 | 主干 N50 | 分支 N50 | 配对差值的 95% CI，ms |
| --- | ---: | ---: | --- |
| native true | 0.249 | 0.244 | [-0.013, 0.006] |
| Host spawn true | 29.552 | 29.437 | [-0.287, 0.053] |
| status | 2.119 | 2.153 | [-0.025, 0.112] |
| review | 2.422 | 2.439 | [-0.055, 0.087] |

四项均未检出差异。顺序套件曾出现 Host 30.54 vs 42.24 ms，
同时 native 从 0.20 变成 0.38 ms；随机配对没有复现该差距。
顺序结果不能解释为分支造成的回归，原始样本保留，不删除。

固定 task 0 的 B-REPLAY prepare-only 随机配对，单位 ms：

| 适配器 | 主干 N60 | 分支 N60 | 配对差值的 95% CI，ms |
| --- | ---: | ---: | --- |
| claude-code | 6.164 | 5.837 | [-0.538, 0.228] |
| codex | 5.976 | 5.819 | [-0.602, 0.340] |
| opencode | 5.929 | 5.984 | [-0.376, 0.576] |
| mini-swe-agent | 5.833 | 6.080 | [-0.212, 0.568] |
| openhands | 5.928 | 6.008 | [-0.548, 0.591] |
| pi-agent | 6.099 | 5.812 | [-0.785, 0.273] |
| swe-agent | 5.902 | 5.829 | [-0.448, 0.266] |

全部 840 个正式命令均通过格式、边界、参数、历史观测、零工具执行及工作区不变校验；
七项均未检出差异。文档独立 task 0 历史批次的 5.56–5.74 ms 使用 hyperfine，
当前为 Python `perf_counter_ns`，不能把二者差值当回归。
这里没有测真实 Agent bridge、模型流、取消中的传输或继续推理。

## 制品与证据

本地原始批次：`benchmark/startup/.data/simplification-eaf11f2f/`。
`.data/` 不入 Git，也不随 checkout 分发；本报告保留结论和关键摘要，不承诺其他机器可直接访问原始数据。

| 文件 | SHA-256 |
| --- | --- |
| 主干静态 pvisor，29,495,968 B | `59c5b4f849a01dec49dee463de352ee6b25aa9dacb07f545637af36a50930b7b` |
| 分支静态 pvisor，29,436,832 B | `817ae17655bdec69d2b069a704497bf057224d3b1fd1acab13781f28ec24ef14` |
| `samples.json`，启动配对 | `f35d3fdb7697c518e8d96a0e5c71573e3e3164d1683277afb7a36ba3b163e8d8` |
| `paired-command-samples.json` | `0a95a8eb09b1093089223dcd2507a031475efad075b118b7274691e6a309e90f` |
| `summary.json`，配对 bootstrap | `454d67f5837505a0cddf8cdd69ae4de337b3401172ce500e427050c1dd3ffdbc` |
| `filesystem-full/records.json` | `9e5215ab92124599b6fda61159beaffffa096824de68c4ee847897c6053383c7` |
| `filesystem-s02-n30/records.json` | `32934a14afe7feebe8ac3d927dac3d13dadcd0e631e00ba9c5bbbd1309c469af` |

制品大小只减少 59,136 B；代码行数下降不能直接换算为内存或运行成本下降。

批次保留源码归档、构建日志、构建收据、固件、rootfs/payload、随机顺序、harness、
semspec 导出、每次时钟和输出、Bundle、内容 oracle 及环境失败现场。
早期 namespace 预检因继承的终端 TMPDIR 在私有 `/tmp` 不存在而失败；
正式 worker 显式设置 TMPDIR/TMP/TEMP。动态构建失败、降级执行和被终端超时中断的
文件系统批次均独立保留，不进入上述统计，也不用于证明产品回归。

执行与校验入口：

```sh
batch="$PWD/benchmark/startup/.data/simplification-eaf11f2f"
# 以下采样入口要求全新的私有输出；已有批次只作审计，不覆盖重跑。
target/semspec/debug/semspec bench --cwd "$batch" "$batch/in.json"
target/semspec/debug/semspec bench --cwd "$batch" "$batch/paired-commands.json"
target/semspec/debug/semspec bench --cwd "$batch/filesystem-full" "$batch/filesystem-full/in.json"
target/semspec/debug/semspec bench --cwd "$batch/filesystem-s02-n30" "$batch/filesystem-s02-n30/in.json"
python3 benchmark/filesystem/docs_retest_analysis.py --rootfs "$batch/filesystem-full"
python3 benchmark/filesystem/docs_retest_analysis.py --rootfs "$batch/filesystem-s02-n30"
python3 "$batch/analyze.py"
mise run test-py -- discover -s benchmark/tests
```

benchmark Python 测试 **122/122 通过**。未运行完整 `mise run test`、macOS/Windows、
网络吞吐、并发密度、RAM/offload、冷镜像或真实模型 Agent 任务。
因此结论只覆盖列出的热缓存、单任务路径，不评价删除的 daemon cluster 或设备能力的替代成本。
