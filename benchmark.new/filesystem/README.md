# 文件系统评估：分层负载与独立实验

<a name="user-workloads"></a>

## 分层用户对照

本页用户实验要回答：同一套仓库检查、增量编译和离线发布任务，选择Native、
pVisor host、staged、CLI VM、Docker、Firecracker或QEMU需要等待多久，
哪些调用形状及资源条件决定差距？
注册为[B-FS-TOOLS](../../benchmark/README.md#b-fs-tools)。
`user_workloads.py`为新用户批次入口，按12机制、4资源边界、3工作流分别采样，
N3新环境、内部三轮，不设干扰门禁，不合成总分。复用冻结负载和工具制品，
重新测量各用户配置；已有guest写回工程A/B数据仍留在设计分析页。
九模式对照覆盖Native、host、staged、默认CLI VM、显式guest写回VM、Docker、
Firecracker stock内核、QEMU q35、QEMU microvm，全部在同一新批次随机交替；
不拼接此前五模式耗时，旧七工具数据保持历史口径。准备、Ready、任务工作、最终同步、
验证和退出分列；完整写任务必须保留文件/目录同步以及最终host独立校验。

```sh
python3 benchmark.new/filesystem/user_workloads.py --prepare NEW_BATCH_UNDER_DOT_DATA \
  --payload-batch PREPARED_V2_PAYLOAD_FIXTURE_BATCH --rootfs PREPARED_TOOL_ROOTFS \
  --artifacts FROZEN_CLI_BUILD_DIRECTORY --samples 3 \
  --docker-setup PRIVATE_ROOTLESS_DOCKER_BUDGET_SETUP_JSON \
  --stock-kernel-receipt VERIFIED_STOCK_KERNEL_RECEIPT_JSON
target/semspec/debug/semspec bench --cwd ABS_BATCH ABS_BATCH/in.json
PYTHON_WITH_MATPLOTLIB benchmark.new/filesystem/user_workloads_analysis.py ABS_BATCH
python3 benchmark.new/filesystem/publish_user_workloads.py --batch ABS_BATCH --check
python3 benchmark.new/filesystem/publish_user_workloads.py --batch ABS_BATCH
```

公开下载只保留当前九模式对照的`levels`、`ready`、`failures`三张CSV、四张SVG及一份
来源摘要；环境点和逐轮明细不进入站点。五模式旧对照仅可输出SVG，其CSV和JSON留在
本地。发布器仍完整验证所有原始记录、五张分析CSV与SVG/PDF图库存，再把完整派生材料
保存在`ABS_BATCH/publication/<namespace>/`。`--check`不创建目录或写入归档；
`--destination-root`只改变公开材料的审阅目录，本地归档始终跟随输入批次。

payload/fixture批次只需完成`workloads_v2_run.py --prepare`，不需要先运行工程A/B；
其准备协议与固定工具环境见[WORKLOADS.md](WORKLOADS.md)。CLI目录须包含
`build-receipt.json`和`bin/{pvisor,pvisor-guest}`。新批次冻结这些制品，
原始payload字节不变；共同静态包装器按执行器实际cwd解析workspace/input、output，
避免绝对宿主路径绕过stage。fixture只读挂载在workspace内，读操作也经过对应文件系统。
Native和pVisor每trial新建user/mount/PID namespace、pivot root、Host socket、Run Bundle和stage，
隔离准备在Ready之外，Ready从执行器exec前标记至共同payload输出。源workspace的output
必须保持空目录，最终输出在stage/upper/output中独立验证。所有原始命令、stdout/stderr、
逐轮记录、namespace挂载证明与失败现场保留，成功的大输出校验后回收。

Docker使用新建私有rootless overlay2 daemon，工具镜像预先导入，输入只读bind，
输出保留在容器可写层；创建容器与输出复制在Ready和任务时钟之外。静态launcher记录
真实root filesystem、payload affinity和父组cgroup，退出后复制输出供共同host oracle。
失败保留日志、inspect和尽力复制的输出，再移除仅该trial拥有的容器。
Firecracker、QEMU q35及QEMU microvm共用同一收据冻结的官方stock内核、模块initrd和
最小PID1，不启动发行版服务。每trial使用私有ext4 root/work磁盘、virtio-blk、2vCPU/
512MiB；root最终只读，input只读bind。FC显式Writeback、QEMU显式cache=writeback，
均保留FLUSH，工作盘正常卸载和VMM退出后由debugfs只读导出给共同host oracle。
这些是各产品存储路径的比较，不是纯VMM开销或同一种文件系统的比较。

私有Docker、容器、观察者及VM共用同一个两核quota/2GiB/零swap父slice。
`prepare_reference_budget.py --cpu-placement affinity`准备全新私有daemon和父slice，
不修改全局委派；本机user cgroup未委派cpuset，daemon和观察者继承CPU0,1 affinity，
容器payload单独设置并记录实际affinity，不能宣称runc早期阶段也有硬cpuset约束。
原五模式入口仍可用`--backends native,host,staged,vm,vm-writeback`独立复现，
但新跨运行时发布必须具有全部九模式，发布到`filesystem-layered-references`新命名空间。

私有预算先独立准备，输出路径须短且位于`.data`，例如：

```sh
python3 benchmark/pvisor/prepare_reference_budget.py \
  --output benchmark/.data/NEW_PRIVATE_DOCKER_BUDGET \
  --memory-mib 2048 --cpu-affinity 0,1 --cpu-placement affinity
```

将其`setup.json`传入`--docker-setup`，测量服务自动加入同一父slice。结束后只停止
该收据明确记录的私有daemon和slice，不操作系统或其他用户的Docker。stock内核收据的
来源要求与准备步骤见[参考内核准备](../../benchmark/pvisor/README.md#explicit-firecracker-kernel-controls)；
使用官方vmlinuz、匹配config、可信extractor及所需原版模块initrd，不借用pVisor固件。

快速上限为每环境90秒。同一配置/case首次失败后，剩余计划样本标记为未执行，其他单元继续；
只有三个独立环境全部有效的单元才有中位数，不用成功轮次补齐失败环境。此超时策略与
最初300秒的中断尝试分批保留。N3不是确认性试验，不提供可靠P95/P99；资源图只有
128/640 MiB ×1/4 worker四个锚点，不证明容量拐点。默认VM与显式guest写回均为普通
CLI路径，host写回关闭、DAX由产品关闭，目录rootfs不声明Immutable generation。

## 后续主协议：机制、资源边界与完整任务

[WORKLOADS.md](WORKLOADS.md) 定义新的 B-FS-ENG 三层评估：12个机制锚点、
4个资源/并发锚点、3个完整工作流。完整任务分别报告，七工具保留为历史应用回归，
不再作为系统性能总分；受益、无收益和退化负载均进入固定目录。

`workloads_v2.py` 保持为离线规格与计划生成器，不运行VM；其零测量字段不代表实际
采集库存。`workloads_v2_run.py` 是独立执行入口：静态Rust payload覆盖19个锚点，
私有fixture冻结后，每case/条件/配对启动新VM。Ready、准备、工作、最终同步、
验证和退出分列；guest逐轮核对，正常退出后host独立核对最终保留内容与逻辑命名空间。
host只隐藏已验证的OverlayFS内部标记，不放过意外应用文件。
默认浅测为guest WB 0/1、host WB/DATA 0，N3配对、内部三轮；不设干扰门禁，
不合成19项总分。N5与单轴扩展仍属后续选择，native/host-WB/lower-DATA执行尚未接入。
逐case guest脏页峰值、队列占用与精确操作尾延迟仍未插桩。
下述旧入口和来源保持独立，已有数据不重命名为新协议结果。

首轮浅测结果见[分层工程分析](../../docs/src/zh/lab-notes/filesystem-io-performance-analysis.md#workload-v2-results)：
19项、114个新VM、342条轮记录全部通过；独立N1计数另用16个VM。
这批数据只比较同一固件的guest WB关/开，不代表0049相对0048的增量或跨后端排名。

```sh
python3 benchmark.new/filesystem/workloads_v2_run.py --prepare NEW_BATCH_UNDER_DOT_DATA \
  --rootfs PREPARED_ROOTFS --firmware PREPARED_FIRMWARE \
  --firmware-source MATCHING_SOURCE_JSON --samples 3
target/semspec/debug/semspec bench --cwd ABS_BATCH ABS_BATCH/in.json
PYTHON_WITH_MATPLOTLIB benchmark.new/filesystem/workloads_v2_analysis.py ABS_BATCH
python3 benchmark.new/filesystem/workloads_v2_audit.py ABS_BATCH
```

`--prepare`仅构建静态负载与SDK driver、冻结私有输入和制品，不启动VM；测量入口
在独立user systemd服务中安装两核/2 GiB/零swap预算，需要宿主KVM与user bus权限。
可用`--cases M06,M07,M10`选择少量完整case，输出目录拒绝复用。
计数必须另外创建`--samples 1 --diagnostic`批次；其整环境请求包括准备和验证，
不能当作正式任务时钟内的逐阶段请求。`workloads_v2_progress.py`与
`workloads_v2_mixed_analysis.py`分别生成worker 0提交进度及混合负载前台读图。
原始数据、失败尝试、冻结代码、执行收据与后处理产物留在`.data/workloads-v2-*`；
不重跑旧campaign，不把三轮当成三个独立环境。

## 缓存与异步写的系统曲线

[CURVES.md](CURVES.md) 定义 B-FS-ENG 的 host-owned 写缓存工程实验：同一个
静态负载在 native 与 VM 中运行，按请求粒度、并发、同步频率、容量、批量和
等待窗口画曲线。WRITE 返回、FLUSH 排空、显式文件/目录 fsync 分开记录，
每个新环境内部三轮；N5 筛选与独立 N30 确认分开。`curves.py` 保存冻结来源、
完整字节校验及原始行，`curves_analysis.py` 导出 CSV/SVG/PDF；
`curves_diagnostics.py` 只接受独立 B-FS-DIAG 批次并严格核对实例与阶段库存。
`curves_cli.py` 用真实普通 CLI 七工具验证落地效果，独立于 SDK 机制曲线。
`read_cache_curves.py` 单独扫描 1–768 MiB 的 Immutable lower 工作集，在 512 MiB
guest 内连续五次 reopen、内部三轮，展示吞吐、READ 消除与容量边界的历史变化。
工程发布器`publish_workloads_v2.py`、`publish_guest_writeback.py`及其tuning、CLI入口、
`publish_io_curves.py`只向站点输出SVG。完整CSV、JSON来源摘要和对应派生图归档在
主输入批次的`publication/<namespace>/`，PDF继续保留在原分析目录；CLI入口没有图时
仅做本地归档。组合I/O曲线以`--shape`批次作为归档主目录。归档清单完整描述材料，
`public_outputs`仅列实际公开的图；阶段计时、来源校验和SVG/PDF完整性检查均不减少。
I/O 曲线发布器只复制当前文章或主题引用的 SVG；未引用的图仍保留在本地 publication。
这些结果不写入下述原生工具表或跨运行时用户排名。

## 原生 metadata/read 表

这里回答的问题：准备好的热缓存输入上，原生 metadata/read 完整 Python 命令需要多久？
注册条目为 [B-FS-TOOLS](../../benchmark/README.md#b-fs-tools)。当前入口只覆盖两项原生探针；
七工具内部阶段、跨后端任务及工程 A/B 的范围仍由各自注册条目约束。

[tools.json](tools.json) 是一张两行表：

```json
{
  "native-metadata": "$ python3 @REPO@/benchmark.new/filesystem/metadata_timing.py @BATCH@",
  "native-read": "$ python3 @REPO@/benchmark.new/filesystem/read_timing.py @BATCH@"
}
```

```sh
just benchmark filesystem/tools
just benchmark filesystem/tools --row native-read
just benchmark filesystem/tools native-read
just benchmark filesystem/tools --output benchmark.new/filesystem/.data/measurement
```

命令直接输出渲染表格，并生成同名 `tools.render.json`。第二个位置参数或 `--row` 只执行选中的行，
更新该行并输出合并后的表格，其他已测行保留。行名忽略大小写和多余空格，未知行会列出可选行。
当前可选行为 `native-metadata`、`native-read`；`pVisor VM` 属于 `startup/timing`，文件系统 VM 行尚未接入。
默认临时目录自动清理，只保留最终表格；显式 `--output` 保留失败诊断和成功审计证据，拒绝复用目录。

## 输入与计时

两行共用一份固定输入：32 个目录、2,048 个小文件和一个 64 MiB 数据文件。
准备时流式生成数据文件，不创建每样本副本。metadata 遍历并统计文件数和大小；
read 读取完整数据并校验 SHA256。输入大小、目录结构及所有内容在计时外核对。

每行固定 3 次预热、30 次正式样本。观察者用 `perf_counter_ns` 在创建子进程前开始，
正常零退出并收集输出后停表。解释器启动、管道收集和负载自身的校验包含在完整命令时间内；
输入准备、观察者启动和后续审计不计时。各条件顺序独立采样，报告样本标准差，不推导配对改善比例。
每次输出必须是对应操作的成功 JSON；失败或超时阻止更新最终结果。

## 代码与证据

- `prepare.py`：核对固定行命令，创建共享输入并执行逐行预检。
- `workload.py`：输入生成、完整校验与两种单次负载。
- `fs_timing.py`：共同采样、时钟和统计；`*_timing.py` 固定操作。
- `audit.py`：校验逐次命令、零退出、单调时钟、样本统计与 semspec 行映射。

保留批次含 `in.json`、`in.render.json`、`in.bench.json`、manifest 和冻结 harness，
逐次日志在 `trials/<mode>/`，正式样本在 `<mode>.samples.json`。
本地 CSV / Markdown 可通过 `update.py` 显式发布到 `.data/results/`。
旧 results 已归档到 `.data/history/`；旧 workload CLI 与 hyperfine 模板不再参与执行。

Python 时钟数据与旧 hyperfine 数据的来源不同。旧数据和撤下的公开文件保留在本地
历史归档，用户页的新分层对照使用独立CSV，不沿用七工具总分；`docs.py publish` 会拒绝时钟范围不匹配的发布。
需要发布新口径时应登记独立文档表与 CSV，不覆盖或拼接旧样本。

## 不可变 share 的 DAX 工程 A/B

`dax_ab.py` 服务 [B-FS-ENG](../../benchmark/README.md#b-fs-eng)。这是独立的
phase-clock 配对实验，不写入 `tools.render.json`，不代表 CLI 可写 workspace。
准备命令冻结源码、优化二进制、固件、静态 C workload 和不可变输入，并提前保存
30 对平衡随机顺序。两组使用相同的只读设备，仅切换 guest 的 DAX 挂载选项。

```sh
python3 benchmark.new/filesystem/dax_ab.py --prepare benchmark.new/filesystem/.data/dax-UNIQUE \
  --rootfs PREPARED_ROOTFS --firmware PREPARED_FIRMWARE
tools/semspec/target/debug/semspec bench benchmark.new/filesystem/.data/dax-UNIQUE/in.json
```

真实测量需要 Linux x86_64 的 `/dev/kvm` 和可安装 CPU/memory 限制的 user systemd。
完整 campaign 位于同一个两核/2 GiB/零 swap cgroup，监测线程在组外的 CPU 2,3。
CPU 0,1 通过继承的 taskset affinity 限定；可用时同时设置 cpuset，缺少控制器时
记录该项为空。宿主 `RLIMIT_NOFILE` 为 65,536，避免 metadata inode 句柄耗尽。
每组 3 次预热、30 个正式新 VM；各阶段完整字节验证、零退出、DAX 属性、预算与
宿主输入不变都是成功门禁。首次访问和重复访问独立统计；重复 read/parallel 为
4 遍的每遍均值，mmap 为 16 遍的每遍均值，metadata/small 各一遍。
小文件 open/read/close 包含正常窗口回收；大文件 read 排除 OPEN，mmap 首次包含
建立映射和逐页首次访问。阶段校验和准备不计时，双线程阶段包含创建和回收线程。
失败拒绝整个批次，不删除有效慢样本。预先登记的分离分布规则、5,000 次配对
bootstrap 和所有计数见 `protocol.json`；干扰门禁是 0.5 秒的外部构建/测试扫描，
不等于排除了全部宿主噪声。原始证据和完整逐次日志保留在批次目录，
`summary.json`/`summary.csv` 与 `in.render.json` 从同一批数据生成。

## VM 缓存逐项验证

[验证报告](VM_CACHE_VALIDATION_REPORT.md)分别记录 rootfs 数据缓存、rootfs/workspace
元数据 TTL 和独立只读 no-open 的工程结果，使用 B-FS-ENG 与 B-FS-DIAG 的注册范围。
CLI 独立确认每臂 N30；快速筛选 N10、只读阶段 N5 与请求诊断分别保留。
完整七工具任务未检出加速，Cargo 有明确改善；只读 no-open 的阶段收益不外推普通
可写 workspace。之后仅将具备物理不可变声明的 rootfs 数据缓存接入默认路径，
元数据长 TTL 和 no-open 仍是原型；用户跨后端页面不使用这些数字替换既有结果。
接入条件和关闭控制见 [VM 合同](../../crates/pvisor-vm/README.md#rootfs-data-cache-default)。

完整精度数据见 CLI 确认（本地归档：`.data/derived-tables-20261018/vm-cache-confirm.csv`）、CLI 筛选（本地归档：`.data/derived-tables-20261018/vm-cache-screen.csv`）和
只读阶段（本地归档：`.data/derived-tables-20261018/vm-cache-readonly.csv`），来源及验收范围见
收据（本地归档：`.data/derived-tables-20261018/vm-cache-validation.provenance.json`）。冻结源码、二进制、输入、运行入口和失败
尝试保留在 `.data/vm-cache-verify-20261012/`；复核使用其中的 `confirm.json`、
`screen-retry.json`、`diagnostic-retry.json`、`ro-screen.json` 和
`ro-diag.json`，均经 semspec bench 执行。复跑必须先为输出配置新的批次目录，
避免覆盖原始证据，并满足报告列出的 KVM、预算和干扰门禁。
