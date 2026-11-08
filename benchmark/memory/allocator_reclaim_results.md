# HVF 宿主释放已修复，整台 VM 的 footprint 实测下降

## Motivation

PID 1 控制的 preload worker 原先已经能执行 glibc malloc_trim(0)，使应用的
64/256 MiB 已释放堆归还 guest 内核，但宿主 VMM 的 footprint 不下降。
短板位于收到 virtio-balloon free-page report 后的宿主释放：原 macOS 分支只有
MADV_FREE 建议，既没有保证立即丢弃页面，也没有检查建议调用失败。
返回 0 不能作为 HVF RAM 已实际释放的证据。

现在普通匿名私有 RAM 采用明确的映射事务：保留报告描述符 → 回调返回 → 暂停全部
vCPU、排空设备访问 → HVF unmap → 原 HVA 上 MAP_FIXED 匿名零映射替换 → HVF remap
→ 恢复运行 → 完成报告。guest 在完成前不能复用这些页；HVA 地址和所有权不变。
仅替换完全落在报告内的宿主页，16 KiB 边缘和未报告数据不动；重叠/相邻段只在同一
RAM region 内合并。文件/COW、内核/设备窗口以及 cold pager/preparation/去重策略
排除，继续各自原有的所有权协议。中途映射失败会终止独立 runner，不恢复部分映射。
设备忙时经已有事件重试，预算两秒后跳过 advisory discard 并确认报告。

调查参考了 [ArcBox 的 HVF 回收复现](https://github.com/arcboxlabs/macos-vm-memory-ratchet)，
其中也提供 munmap/MAP_FIXED 路径并讨论 reusable 状态的计费问题。本实现采用匿名
零映射替换，未采用 MADV_FREE_REUSABLE 私有建议或强制内存压力。以下数值均来自
本机独立探针及 pVisor 实际 VM，不引用该项目的性能数字。

## 实验设计

工程实验登记为 B-ALLOCATOR-RECLAIM-ENG，未发布为用户 benchmark。
宿主 Darwin 27.0.0/arm64、24 GiB RAM、Apple Silicon HVF；每台新 VM 配置
512 MiB/2 vCPU，最小 Ubuntu 20.04/glibc 2.31 userspace。release runner 使用公共
VM API 启动当前嵌入的 PID 1，经真实 vsock Start/ReclaimAllocator 控制同一 SO。
初始 idle 进程和 payload 都计入同一个 VMM。无 cold pager、offload、去重或全局
缓存清理；未设 CPU 亲和性/cgroup。观察期间持续拒绝其他 VM/编译器干扰。

四个线程在单 arena 分配并逐字节校验 64/256 MiB 数据，随后 join；free 后保留
64 KiB 活跃 guard。关闭自动 trim 和 mmap 分配，使 freed heap 在操作前仍驻留。
对照与实验均加载同一 SO，分别请求 inspect 或 malloc_trim(0)。每轮四条件预定
随机排序，各条件使用新 VM；五轮共 20 台，每个条件 N=5。

free 后记录五秒基线，请求后记录十五秒；外部 Python 观察者每 250 ms 调用
proc_pid_rusage，阶段末两秒样本中位数定义阶段读数。主指标是整个 VMM 的
ri_phys_footprint，同时保存 resident size 和 guest MemAvailable/RssAnon。
它覆盖 VMM 及其 guest RAM，未覆盖不归属该进程的宿主文件缓存或全部内核成本。
Footprint 保留 dirty/compressed/swapped 计费，RSS 单独下降不能证明成本消失，
见 [Apple 指标说明](https://developer.apple.com/videos/play/wwdc2022/10106/?time=1012)。

观察后重新分配相同数据并全量校验，记录宿主占用；保留分配，再执行同条件的
trim/inspect，等待三秒，重新逐字节校验全部活跃块和 guard，payload 退出 0；
随后受控终止 idle，由 PID 1 认证 VM 退出 143。这个新增正确性尾段不进入主窗口，
原 64/256 MiB、五/十五秒、随机五轮的主要计量协议不变。目标进程在测量期保持运行。
本实验不将 join 后的回收描述成四个分配线程同时运行的压力测试。

修复前 ar-f1 和修复后 ar-fix-f1 分开保存，未拼接样本或做跨批次配对。
payload、SO、glibc/loader 和固件摘要与旧批次一致；变更为 VMM 的宿主释放策略，
以及主窗口之后的正确性尾段。所有阶段原始曲线、失败证据、来源和制品均保留。

## 数据与分析

2026-11-08 修复后正式批次 ar-fix-f1：20/20 VM 完成，零有效样本剔除，所有完整性、
退出及 454 项冻结源码/制品摘要校验通过。下表每格是该条件五个 VM 阶段读数的
中位数，单位 MiB；前后两个中位数之差不替代逐轮配对收益。

| 已释放堆 | 条件 | VMM footprint 前 | 后 | 重新分配后单次读数的中位数 |
| --- | --- | ---: | ---: | ---: |
| 64 | inspect | 108.563 | 108.595 | 108.720 |
| 64 | trim | 108.517 | 56.173 | 108.642 |
| 256 | inspect | 300.923 | 300.985 | 301.048 |
| 256 | trim | 300.923 | 57.001 | 301.064 |

“额外减少”按每轮 (trim前−trim后) − (inspect前−inspect后) 定义；正数表示额外宿主
收益。下表报告配对中位数和 5,000 次配对 bootstrap 95% 区间，单位 MiB，N=5：

| 已释放堆 | footprint 额外减少中位数 | 95% 区间 | 五次配对值 |
| --- | ---: | --- | --- |
| 64 | 52.359 | [48.953, 55.891] | 48.953, 53.969, 52.359, 55.891, 49.906 |
| 256 | 243.953 | [242.078, 246.203] | 242.578, 246.203, 242.078, 243.953, 244.063 |

64/256 MiB trim 条件的 guest RssAnon 中位数分别由 64.219/256.219 MiB 降至
0.223/0.227 MiB；inspect 的 guest RssAnon 保持不变。与旧策略不同，这次 whole-VMM
footprint 同时下降，且 inspect 没有相应收益。回收不保证在十五秒内等于全部 freed
heap：本路径只处理 guest 实际报告且宿主页完整的范围，剩余差额没有逐页归因。

全部十个 trim 样本重新分配后，footprint 回到各自基线以上约 0.063–0.172 MiB，
并非 RSS 回升而 footprint 仍维持低值的计费现象。重新分配、活跃数据再 trim 后的
全量字节/guard 检查和正常退出全部通过。没有在验证尾段制造宿主压力。

RSS 的分离样本仍存在，inspect 也出现大幅下降。因此保留五次原值，不以一个
总体 RSS P50 描述效果。正数表示下降、负数表示增加，按轮次排序，单位 MiB：

| 批次 | 已释放堆 | 条件 | 五次 VMM RSS 减少值 |
| --- | --- | --- | --- |
| 修复后 | 64 | inspect | -0.031, -0.047, 48.539, -0.062, 78.477 |
| 修复后 | 64 | trim | 48.906, 53.922, 90.672, 84.078, 65.031 |
| 修复后 | 256 | inspect | -0.047, -0.094, -0.062, 153.984, 214.906 |
| 修复后 | 256 | trim | 242.453, 168.438, 265.109, 275.688, 243.906 |
| 修复前 | 64 | inspect | 82.156, -0.047, -0.031, -0.047, -0.047 |
| 修复前 | 64 | trim | -0.078, -0.109, -0.109, -0.109, -0.125 |
| 修复前 | 256 | inspect | -0.062, -0.062, 49.234, -0.016, -0.062 |
| 修复前 | 256 | trim | 28.484, -0.141, -0.117, -0.141, -0.125 |

这与宿主压缩/换出影响 RSS 一致，但未逐页证明每个离群值的成因；不把 inspect 的
RSS 下降归因于 malloc_trim。修复后 footprint 配对收益在五轮内持续为正。

修复前 ar-f1 的独立历史结果如下，单位 MiB。它也为 20 台独立 VM，全部完整性及
退出通过，但没有新增三秒 live-reuse 尾段；不与新批次合并估计：

| 已释放堆 | trim VMM footprint 前 | 后 | footprint 配对额外减少中位数 | 95% 区间 |
| --- | ---: | ---: | ---: | --- |
| 64 | 108.563 | 108.657 | -0.0625 | [-0.0781, -0.0312] |
| 256 | 300.954 | 301.079 | -0.0781 | [-0.1094, -0.0625] |

旧 inspect 的 footprint 为 108.548→108.579、300.985→301.017 MiB。旧 trim 的
配对值 64 MiB 为 -0.0312,-0.0625,-0.0781,-0.0625,-0.0781；256 MiB 为
-0.0625,-0.0781,-0.0625,-0.1094,-0.0781。小幅增加不能称为节省。
旧日志诊断 ar-d1 确认收到 124 段、累计范围 468 MiB 的 free-page reports，其中
包括启动空闲页及可能重复范围，绝不是 468 MiB 的物理节省。诊断未并入正式样本。
旧 runner 未设置 ram_backing，使用默认匿名私有 RAM；此前将它误述为文件 backing
的背景判断已纠正，原始测量不变。

独立原生 HVF 探针没有 vCPU，只证明 host-dirtied 映射机制；完整 guest-dirtied
链路由上面的真实 VM 批次验证。探针给 64 MiB 非零数据加两侧活跃 guard，检查
所有字节、零映射及 refill；原始结果单位 MiB：

| 探针阶段 | footprint | RSS |
| --- | ---: | ---: |
| filled-mapped | 65.860 | 69.719 |
| mapped-free | 65.860 | 69.719 |
| unmapped-free | 65.860 | 69.719 |
| remapped-free | 65.860 | 69.719 |
| refilled | 65.860 | 69.719 |
| zero-replaced | 1.844 | 5.719 |
| remapped-zero | 1.844 | 5.719 |
| refilled-verified | 65.860 | 69.719 |

本机 mapped MADV_FREE 以及 unmap/MADV_FREE/remap 都没有立即降占用；匿名零映射
替换立即下降约 64 MiB，remap 保持低占用，写回后正常重新计费且数据完整。
这不证明 MADV_FREE 在所有压力/系统版本下都无效，也不推断闭源 AppleHV 的具体
pinning 实现；修复直接消除对延迟建议和 reusable 账本状态的依赖。

保留证据：`.data/ar-f1/`（旧正式）、`ar-fix-f1/`（新正式）、`ar-fix-p1/`（新四格
成功预检）、`hvf-fix-probe/`（独立诊断及摘要）。旧 ar-p1/ar-p2 是启动失败预检，
ar-p3 为成功预检；全部与正式批次隔离。每个正式目录含 manifest.json、冻结源码、
samples.json、summary.json、每 VM 的主窗口曲线/guest 协议帧/计数/日志；新批次
另含 live-reuse.jsonl 和重新分配计数。旧 453 项和新 454 项冻结输入摘要均已核验。

验证：`mise run test -- pvisor-vm -- --lib --tests --no-fail-fast` 在本机真实 HVF
权限下 447 通过、3 跳过；新增范围、排空与延迟完成报告测试通过。pvisor 消费方
编译、VM crate 的 clippy --no-deps 和 rustdoc -D warnings 通过；全依赖 -D warnings
被既有 overlay-core 两个 unused-variable 告警阻挡，未修改无关代码。
观察者四项与注册/计时框架二十项测试、Ruff 和新增模块 rustfmt 检查通过。
直接 Linux musl 交叉检查因缺少 x86_64-linux-musl-gcc 未完成，Linux 分支策略未改。

正式批次源于 53b6930d 和当时工作区修改；manifest 记录了 dirty 状态，精确实现以
冻结 sources/ 为准。PR 从最新 main（84b0898a）独立提取内存相关修改，另构建 release
runner 完成 ar-pr-p1 四格真实 VM 预检，完整性和退出全部通过。该预检中，
64 MiB trim 的 VMM footprint 为 108.438→56.548 MiB；
256 MiB trim 的 VMM footprint 为 301.017→59.048 MiB。预检只确认独立 PR 构建仍工作，未并入正式
五轮估计。PR 分支另通过 12 项 guest 契约、四项观察者、二十项注册框架及消费方编译；
VM 初次并行测试一个既有 VmCreate 用例失败，随后串行整套 447 通过、3 跳过，
未修改该用例。clippy --no-deps 和严格 rustdoc 同样通过。

限于本台 Apple Silicon、普通匿名私有 RAM、单 arena、十五秒观察窗；不外推生产
Agent/多 arena 碎片、其他 macOS、KVM 或压力下的效果。未测长期重复回收的 VMA
碎片/暂停成本或性能退化；文件/COW/cold pager 实例不进入新替换路径。
复现与工具构建见 [实验手册](README.md#pid-1-allocator-reclaim-and-whole-vmm-memory)。
