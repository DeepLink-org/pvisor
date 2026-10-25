# 启动实验：先看协议，再运行并保留证据

这里测量预准备环境从进程创建到首条正确输出的 Ready 时间，并保留同一次启动的正常 Exit。
[公共协议与复现步骤](REPRODUCING.md) 说明工具版本、输入来源、测量边界、成功判据、
原始证据和已验证范围；从干净源码目录先走 Native，不依赖本机历史资产。
注册需求见 [B-STARTUP](../REGISTRY.md#b-startup)。

## 运行与审计

```sh
mise trust
mise install --locked
mise run test-startup
out=benchmark/startup/.data/native-repro-01
mise run benchmark -- startup/native_timing --output "$out"
mise exec -- python3 benchmark/run.py --audit "$out"
```

输出目录必须不存在。每次实验换一个新目录；省略 `--output` 会在审计后清理临时证据。
`--audit` 核对保留记录并重算统计，不执行测量、不发布数据。
整次选择的完成情况记在 `run.json`；失败、缺行或修改过的证据不能作为完整实验。
模板旁的预览表可能保留其他批次的行，复核以证据目录为准。

## 实验入口与当前范围

| 入口 | 内容 | 当前复现范围 |
| --- | --- | --- |
| [timing.md](timing.md)、各 `*_timing.md` | 八种基本后端的定义与采样参数 | Native 已验证；其他后端需要外部参考输入和运行环境，尚未验证干净环境复现 |
| [image_compare.md](image_compare.md) | 同源预准备镜像的三组配对 | 专用脚本协议；依赖预准备 lab/store，未验证干净环境复现 |
| [image_lazy_compare.md](image_lazy_compare.md) | 冷暖客户端的三组配对 | 另需已验证的 warm-batch 收据；未验证干净环境复现 |
| [stage_lazy_compare.md](stage_lazy_compare.md) | 相同 staged 执行器的两组文件加载 | 另需匹配 runtime rlib 和私有挂载环境；未验证干净环境复现 |
| [checks.md](checks.md) | 输出、超时、完整链路、缺失输入与诊断契约 | `mise run test-startup` 通过 semaspec 执行，新规格保持未审核 |
| 其他注册 Markdown | 初始化、内核、SDK 等实验需求 | 通用入口尚未准备，不能用 null 表或历史值冒充测量 |

## 当前运行链路

`Markdown → semaspec 计划 → 准备与预检 → semaspec bench → 逐次观察与校验 → 独立审计`

八份 `*_timing.md` 定义 argv、cwd、环境、采样次数和超时。三个镜像配对实验的
Markdown 定义单次脚本入口，其分组、随机顺序、缓存和采样协议由专用 Python 脚本拥有。
`common.py` 准备输入，`timing.py` 调用公共 Ready/Exit 观察器，`hooks.py` 校验输出和
Bundle，`audit.py` 重算全部样本；`publish.py` 只负责已经审核的公开数据更新。
具体字段与输入限制以公共协议和对应实验为准。

以下保留历史证据范围与原有链接锚点。原始路径与摘要代表当时的实验身份，
不会自动提供新 checkout 所需的资产，也不随新协议改写。

## OpenSandbox 十请求突发证据 {#opensandbox-burst-evidence}

问题：相同完整产品预算下，pVisor 与官方 OpenSandbox 同时接收十个创建请求，
整批创建、首命令完成、失败和内存表现如何？此对照属于
[B-OPENSANDBOX-ENG](../REGISTRY.md#b-opensandbox-eng) 的低内存突发协议，
独立于单请求启动和 pVisor 单臂突发诊断，不通过未准备的 `opensandbox.md`
或 `opensandbox_burst.md` 重新测量或发布。

2026-10-24 批次保存在本地 `.data/opensandbox-pvisor-burst-ab-20261024/`，
包含完整预检、预热及正式批次，不随 checkout 或站点分发：

- `REPORT.md`、`summary.json`、`validation.json`：工程报告、配对统计和正确性汇总。
- `formal/receipt.json`、`formal/*/result.json`：全部逐请求纳秒时钟、内存采样和资源记录；
  `formal/formal-06-docker/` 保留官方慢批次，没有筛除。
- `setup/`、`preflight/`、`formal/`：服务日志、配置、输入准备、独立预检、删除及组回收记录。
- `artifacts.json`、`upstream-receipt.json`：制品与上游来源；冻结源码、二进制、镜像、
  锁文件及 `harness.py`、`support.py`、`analyze.py` 一并保存。

中英文结论、实验边界及数据表见
[OpenSandbox 工程分析](../../docs/src/zh/lab-notes/opensandbox-performance-analysis.md#burst-10)。
`summary.json` SHA-256 为
`54dd0f400c89f504f56aef7ea910ccf325dd4db272a497992b6c37381c114ccb`。
2026-10-25 从 `benchmark.new/startup/.data/` 迁入当前目录时逐文件核对原字节，
完整索引保存在旁边的 `.data/opensandbox-pvisor-burst-ab-20261024.relocation.json`。
原始收据及冻结 harness 中的绝对路径保留历史值；离线复核时应将旧批次根映射到当前根，
不能直接运行冻结采样器来冒充新的受支持测量入口，也不应改写原始证据中的路径。

## TurboOCI 按需镜像对照

[TurboOCI 对照协议](TURBO_OCI.md)属于 B-LAZY-STARTUP，分别比较客户端准备、
Ready、正常退出和传输成本，与上面的已准备环境热缓存启动表保持独立。
独立 OverlayBD/TCMU 环境已部署，操作和功能验证见
[部署记录](OVERLAYBD_LAB.md)。`lazy_startup.md` 仍为未准备的占位表，
正式四组采样与审计尚未接通，不发布新成绩。

```sh
python3 benchmark/startup/turbo_oci_preflight.py --help
python3 benchmark/startup/turbo_oci_preflight.py --binary-dir target/release
```

预检只读取设备、工具及制品状态，不安装依赖、加载内核模块、启动服务或清理缓存。
只有所有前提通过并完成专用采样与审计接线后，才能执行正式对照；
预检通过本身不证明挂载、镜像导入或隔离已验证。

`PREPARED_IMAGE_STORE` 必须替换为通过公开 cache API 准备并验证的镜像 store。
历史 `.data/` 路径只标识原始证据，不是新 checkout 自带的输入。

### 已准备镜像的性能对照

`image_compare.py` 比较同源 Ubuntu shell 在普通 OCI 容器、TurboOCI 容器和
pVisor 本地预准备 rootfs VM 中的 Ready / Completion；不包含镜像拉取、转换或
解包。每组 3 次预热、30 次随机交替正式启动，专用审计复算时钟、Bundle、
制品、清理和后台负载，生成本批次的 Markdown/CSV。

准备、执行和审计步骤见 [image_compare.md](image_compare.md)；旧 `.data/` 路径只标识历史证据，不能作为新 checkout 的输入。

实验目录必须全新，不能并行复用同一 lab。采样包含容器的 Docker exec 客户端，
VM 的 2 GiB guest RAM 与容器硬限制不代表等整机内存预算。宿主页缓存保留，
其他 CPU 上的后台负载单列；不清理或停止其他测试。
结果保留在 `.data/image-compare-prepared-20261017-a/`，含 99 次完整启动证据、
专用审计和 `report.md`；这是独立 prepared scope，不填充仍未准备的四组
`lazy_startup.md`，也不通过通用 `update.py` 发布为用户 benchmark 表。

### 相同 stage 执行器的惰性文件加载 {#stage-lazy}

`stage_lazy_compare.py` 将两种按需 lower 都交给同一个 pVisor host staged
执行器，宿主工具按相同顺序读取并校验四个镜像文件，共 2,575,738 B。
一侧是公开 cache API 的 host FUSE，另一侧是 TurboOCI/OverlayBD 的只读
ext4 挂载；两侧均使用相同 Docker exec 包装器、CLI、探针与应用 CPU。
它不执行镜像内的程序，不代表 CLI 已支持 host executor 的 OCI rootfs。
`--stage` 只暂存工作区；不能用 `--executor host --rootfs image=…` 冒充这个路径。

准备、执行和审计步骤见 [stage_lazy_compare.md](stage_lazy_compare.md)；旧 `.data/` 路径只标识历史证据，不能作为新 checkout 的输入。

`--runtime-rlib` 指定现有 release 构建的 runtime 库，仅链接 benchmark owner
helper，不改产品 API。准备时核对原始 OCI TAR 中的文件摘要，冻结工具和
harness。正式入口为单个 semaspec Python 配对行，3 轮预热、30 轮随机配对；
每组冷客户端后紧接热客户端，每次均使用新挂载和新 stage。只重置本 lab 的
私有客户端状态，保留 registry 源内容及宿主页缓存；两种服务均在本机，无 WAN。
OverlayBD 使用官方带 lease 的 `images mount/unmount`，并在必要时正常卸载
父挂载、通过官方 attacher 释放对应设备。每次退出后重新审计设备清单。

完整 Ready/Completion 包含客户端打开或 rpull、挂载、stage CLI 和正常退出，
另记录已挂载后的 stage Completion。应用固定 CPU 12,13，镜像服务固定
14,15；没有声明等整机内存预算。任何校验、退出、冷暖取数、实际后端、清理
或预定干扰门禁失败均使整批无效，不剔除慢样本。原始证据和报告保存在该
batch 下，不与原 VM/容器批次拼接，也不通过通用发布器覆盖用户表。

已完成批次位于
`.data/overlaybd-lab-20261017/stage-lazy-20261017-a/`，包括 132 次执行、
逐次原始日志、`audit.json`、`summary.json`、`samples.csv` 和 `report.md`。
独立离线审计通过，并已核对 semaspec 导出与复算结果一致。
正式输入、两种原始/派生 OCI 身份和 helper 链接收据均保留在该批次；
功能预检失败记录留在独立目录，未混入正式数据。

## 启动瓶颈诊断（B-STARTUP-DIAG）

问题：同一条启动命令中，frontend、owner、runner、VMM 与 guest supervisor 哪个阶段最值得继续优化？
`startup_profile.py` 独立采集小批诊断，保留每次原始日志、实际命令、源码工具及二进制摘要，
输出阶段耗时中位数、范围、出现试次和调用数。目录必须全新；每次使用独立 Run store。
默认连续 5 次，无预热，不作为用户启动水位，也不跨批次推算改善比例。

```sh
cargo build --locked --release -p pvisor-cli --bin pvisor
python3 benchmark/startup/startup_profile.py --collect /tmp/pvisor-startup-host-new -- \
  target/release/pvisor spawn --no-agent-defaults --overlaynet off --stdio inherit -- \
  /bin/sh -c 'printf "PVISOR_STARTUP_READY\n"'
python3 benchmark/startup/startup_profile.py --collect /tmp/pvisor-startup-vm-new -- \
  target/release/pvisor spawn --no-agent-defaults --overlaynet off --stdio inherit \
  --vm --rootfs /absolute/prepared-rootfs --cpu 2 --memory 128MiB -- \
  /bin/sh -c 'printf "PVISOR_STARTUP_READY\n"'
python3 benchmark/startup/startup_profile.py /tmp/pvisor-startup-vm-new/trial-*/stderr.log
```

动态构建需追加 `--vm-library-dir /absolute/firmware-directory`；静态构建按既有构建流程
内嵌固件。macOS 需要已准备的 rootfs 和 HVF entitlement，不能使用 Linux host rootfs。
采样前准备好制品/固件/输入，使用空闲且固定的工作区；负载必须输出唯一、精确的 Ready 行
并正常零退出。默认超时 30 秒，失败立即停止并保留现场。此工具校验观察协议，不代替
B-STARTUP 的 Bundle、隔离和内容校验；不要用任意 echo 冒充真实应用已可用。

- `ready`：Popen→首条 Ready 的宿主观察，包含管道和调度延迟。
- `handoff`：通过 owner 绑定定位的创建/动态加载/owner 校验/bootstrap 总间隔。
- `gap`：同一 PID 的相邻 checkpoint 间隔，到本进程启动边界为止，排除执行/收尾；包含日志和调度，也可能跨异步任务。
- `span`：显式内部阶段，使用各自 process-local epoch；guest epoch 独立于 host，与 gap、父 span 可能重叠，不能相加。

`summary.json` 保留完整观察与原始日志摘要。缺失阶段不会补零，未知结果不视为成功。
只有原日志时，没有 Ready observer 的窗口和未标记时间保持未知。
`VMM built→Ready` 包含 ready callback、设备事件循环、guest 和输出传递，不能当作纯内核时间。
进一步确认优化收益时关闭 `PVISOR_STARTUP_PROFILE`，设置 `PVISOR_STARTUP_TIMING=0`，
按 B-STARTUP-ENG/REVISION-ENG 的同输入随机配对流程另测。

首次 RAM 高编号 FD 复制的独立微诊断（Linux）：

```sh
rustc --edition=2024 -O benchmark/startup/fd_probe.rs -o /tmp/pvisor-fd-probe
/tmp/pvisor-fd-probe single
/tmp/pvisor-fd-probe threaded
/tmp/pvisor-fd-probe preallocated
```

每次新进程调用，三种模式交替各跑至少五次，保存全部 JSON 原值。
`first_ns` 是首次 `F_DUPFD_CLOEXEC >= 202`，`second_ns` 是已扩展表后的第二次复制；
`preallocated` 在线程创建前先扩展并关闭该 FD。保留高 FD 的防 `dup2` 冲突约束，
不得从诊断直接推导降低固定 FD 编号是安全优化；该微诊断不启动 VM，不是产品收益对照。
