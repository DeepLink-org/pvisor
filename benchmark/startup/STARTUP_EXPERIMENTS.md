# 启动优化、竞品复测与预热可行性记录

## Motivation

pVisor 的冷启动应先评估重复工作，再判断预热是否值得引入新的资源和生命周期
成本。本轮评估了 Linux x86_64 非 TEE 的 backed VM 省去内核中间复制的候选，
但未检出端到端启动收益，因此未纳入产品代码。预热也仅作为原型评估，
没有引入产品预热池或快照机制。

以下是现有 B-STARTUP-ENG / B-STARTUP-DIAG 下独立工程批次的归档摘要，
不是用户 benchmark 页或“市场最快”排名。实验基于 `705ba52d` 及当时冻结的
工作树；诊断、微探针和 warm-init 原型只存在于实验快照，不是当前产品接口。
PR 只保留实验记录，不包含运行时代码变更、新增测试、临时开关、采样/分析脚本、
原型或对实验入口/registry 的临时修改。历史数据没有在整理 PR 后重新采样，
也没有覆盖 `docs` 的既有用户数字。

## 实验设计

### 共同条件与不能合并的边界

宿主为 Fedora / Linux 7.2.11-300.fc45 / Ryzen 7 9700X，Linux/KVM。
各正式批次分别固定 CPU 12,13，通常请求 2 vCPU / 128 MiB，保留宿主页
缓存，不称冷磁盘。没有完整组 cgroup 预算、固定频率或外部干扰门禁，
因此是探索性结果，不给可靠 P95/P99、生产密度或跨机器结论。

每臂 3 次统计预热、30 个正式试次；有对照的批次预先 seeded 随机交替，
差值按同轮样本计算，bootstrap 5,000 次。每次都是新 VM，不删除有效慢样本，
不以失败的短退出时间填充结果。插桩诊断与关闭插桩的性能采样独立。

| 批次 | 计时与校验范围 |
| --- | --- |
| 内核复制 A/B | 同源 musl/gateway release、同固件和 rootfs；Popen→shell Ready/正常 Exit；Bundle、隔离、空 upper、canary 和输入/制品不变 |
| smolvm local-directory CLI | 同 rootfs/shell；包含其 CLI 内目录导入/状态处理；不代表 prepared pack 或纯 VMM |
| smolvm prepared pack | 官方 v1.32.4 `pack create --image`，预准备磁盘/运行时但不恢复 RAM；同 shell，正式采样复用已展开 assets |
| Microsandbox release | 官方 v0.7.11、v0.6.10 及各自固件/agentd；每 trial 同源独占 rootfs 副本，计时外全量验证；token、CPU online、boot ID、正常退出及无 running 实例 |
| 本机 NVX | 官方 `v0.1.0-dev.ffd79a10b65b` 的 OpenVMM→Alpine boot marker；自身 initramfs，不是同 rootfs CLI 对照 |
| warm-init 原型 | 相同 SDK runner/可信 PID 1/固定 rootfs；cold 请求后 boot，warm 预先 boot 到屏障；都计入请求后输入准备、BIND/START、应用输出和正常退出 |

pVisor 使用 libkrunfw 5.6.2 / Linux 6.12.109。MSB v0.7.11 和 v0.6.10
实际 guest 内核分别为 6.12.113、6.12.99；NVX 为 Linux 6.18.38 /
Alpine 3.24.1。存储、内核、审计与持久化保证不同；CLI Exit 不等于各产品
完成了相同的持久化屏障。不同批次的 pVisor 数字不能相减来推断版本退化。

### 原型的额外限制

warm-init 是私有协调器与静态可信 PID 1 的一次性原型，没有正式 daemon
admission/authentication、host runner sandbox、Core/stage policy 或 Bundle。
固定设备指向独占空 landing 目录，不热换已使用的 filesystem backend。
请求之后才生成 nonce 绑定数据，guest 才挂载、逐字节校验并确认 BOUND；
收到匹配的 START 才执行固定 shell/Python。每 VM 单次使用后销毁。

它只处理公开确定性 fixture，不接受任意租户命令或请求后 rootfs。与完整
CLI 的约 127 ms 不是同一测量边界，不能直接相减预测 daemon SLA。测试
plumbing、镜像/制品准备共同排除；preboot 和 standby 成本另外报告。

## 实验数据和分析

### 1. 未采用的候选：消除内核中间复制

原路径为“内嵌内核解包 → 可写中间映射 → guest RAM”。backed Linux VM
候选实现直接从持有强引用的不可变字节复制到 guest RAM，不将共享源注册为
可写 guest 内存；无 backing 等其他路径不变。该实现已从 PR 撤回，
产品保留原加载路径，以下数字仅描述当时冻结的实验制品。

| 指标 | 基线中位 ms | 候选中位 ms | 配对候选−基线 ms [95% CI] |
| --- | ---: | ---: | --- |
| shell Ready | 113.853 | 113.150 | −0.213 [−2.953, 1.663] |
| 正常 Exit | 183.179 | 182.371 | −1.913 [−8.844, 1.416] |

66 次均通过。独立 N5 插桩确认约 1.809 ms 的 `kernel_owner_copy` 不再出现，
但端到端区间跨零：**确认减少复制，不宣称已经检出整体启动加速**。
实验快照另外验证了所有权释放、非法替换保留原内核、完整字节和 guest
写入不修改共享源；这些临时验证代码不随 PR 保留。

### 2. 冷启动竞品：按批次独立解释

每格为本批中位数，单位 ms；不把本表所有行当成同一个排名：

| 同批条件 | Ready | CLI Exit |
| --- | ---: | ---: |
| pVisor / local-directory 对照 | 115.966 | 181.134 |
| smolvm v1.32.4 / local-directory | 271.347 | 271.772 |
| pVisor / prepared-pack 对照 | 118.934 | 188.998 |
| smolvm v1.32.4 / prepared pack | 1120.532 | 1159.338 |
| pVisor / MSB release 对照 | 127.095 | 204.675 |
| Microsandbox v0.7.11 | 212.530 | 253.226 |
| Microsandbox v0.6.10 | 174.677 | 2196.647 |

smolvm local-directory CLI 每次包含目录输入处理。pack 正式试次的 debug
诊断确认 assets 已展开，agent-ready 约 0.3 s 也不等于完整 shell 输出
Ready；不把全部差距解释成内核启动。两份 smolvm 对照各 66 次通过。

MSB 三条件共 99 次，boot ID 全部不同，正确输出、两个 CPU 在线及退出后
running 查询等检查全部通过。Ready 的配对 pVisor−v0.7.11 为
−83.732 ms [−88.049, −78.489]，pVisor−v0.6.10 为
−45.454 ms [−52.073, −41.144]。两版各一个 >300 ms 的 Ready 慢点保留，
不将孤立点当稳定第二峰。v0.6.10 的 `Exit−Ready` 中位为 2022.700 ms，
额外约两秒在收尾，不属于启动。

MSB v0.7.12 已是官方非 prerelease；补齐匹配 agentd 后仍报 ENOENT，
没有进入性能分布。v0.6.10 不支持 `--no-stdin`，用观察者的 DEVNULL
实现同样的关闭 stdin；tar 执行权限按其官方 installer 的 755 规则准备，
未修改二进制。SONAME 相同没有被当成跨版本混用固件的理由。

本机 NVX 独立 N30 的 OpenVMM→Alpine marker 为 240.872 ms
[225.721–270.038]，正常 guest Exit 为 268.892 ms。该 Ryzen 缺少
built-in CPU profile，使用 host development profile；预检的约 27.4 ms
CPU 指纹成本保留，不能从中位数中直接相减。33 次通过 CPU online、token
和 native exit；仍不等于完整 NVX acceptance，更不能与 pVisor CLI 计算
加速比。ArcBox 的当前 macOS/M3 sandbox 路径未在本机 Linux 测量；
sandstorm/forkd 的 warmed-parent snapshot headline 排除在冷启动对照外。

### 3. 未采用的方向与诊断

- 完整 kernel-ring N3 中，到运行 `/init.krun` 的 printk 时间分别为
  52.703、35.142、33.737 ms。日志明确报告 delay-loop 校准已跳过；
  较慢项包括 jitter entropy、virtio、字符设备和 clocksource 初始化。
  没有据此禁用 entropy/crypto 检查或安全缓解。临时日志开关及新增
  guest 分段未作为产品功能保留。
- 将 release SHA-256 实现改为 opt-level=3，没有显示收益，撤回。
  流式 1 MiB/64 KiB 微探针约 12.532/12.432 ms；只读映射上的 SHA
  约 11.574 ms，但 open/mmap 不在这个阶段时钟内，不是等边界优化证据。
  保留完整内容流式校验，不引入路径/mtime 身份缓存。
- sealed-FD 原型通过 seal、内容、写入拒绝和生命周期检查。owner 建立
  加 child 映射/哈希相对直接解包/哈希的配对阶段差为 −0.114 ms
  [−0.472, 0.300]，未检出净收益，也不是 VM Ready。未增加产品 kernel
  FD、共享服务、快照格式或公共 API。

### 4. warm-init：收益约一次 VM boot，不是应用预热

每格 N30、中位 ms；配对差为 warm−cold：

| 负载 | SDK Cold | SDK Warm | 配对差 ms [95% CI] |
| --- | ---: | ---: | --- |
| 4 KiB + shell | 67.789 | 15.322 | −52.476 [−53.487, −49.816] |
| 4 KiB + Python | 237.853 | 189.088 | −50.310 [−54.859, −43.814] |
| 1024×4 KiB 全量校验 + shell | 645.644 | 600.685 | −46.777 [−61.775, −33.026] |

198 个 fresh VM / 180 个正式样本全部通过，boot ID 均不同，无强制清理
冒充成功。各条件 preboot 中位约 52 ms，warm 是把它前移，不是消除启动
CPU 或自动增加吞吐。

小输入的 warm BIND/挂载/校验/IPC 约 1.8 ms，shell START→输出仍约
13.4 ms。Python 在请求后才启动/import，warm 不能消除它的应用成本。
大目录场景故意包含 guest 全量读回；其约 570 ms 的 BIND 阶段不能被
解释成“晚挂载本身需要 570 ms”，生产可信 generation/索引路径未必需要
每次扫描，须另测。

约 250 ms standby 窗口内，单 VMM 的 PSS 中位 51.549 MiB
[51.195–51.680]；CPU 计数精度为 10 ms，中位 0、最大增加 20 ms，
不能声称待机 CPU 为零。独立 N1 五秒 idle 增加 20 ms CPU，之后正常
处理请求。它不是完整 daemon/cgroup 计费或整机净新增内存。

五个独立负例覆盖取消、控制 EOF、错误校验码、旧 nonce 输入与错误 START，
均为预期 native exit 125 且没有执行应用。尚未覆盖生产 policy、并发竞争、
daemon 恢复、池 miss/突发/补充和完整资源预算。

**建议：** 值得继续固定镜像、固定规格、一次性消费的小池 opt-in 实验，
但不默认启用，不复用运行过用户代码的 VM，不宣称“最快预热”。原型与临时
验证均不随这个实验记录 PR 进入产品。

### 5. 证据与来源

完整制品、输入、冻结入口、原始输出、纳秒记录、失败现场和旧验证代码
保留在本地忽略的 `benchmark/startup/.data/`，不随 checkout 分发：

| 归档 | 摘要文件 SHA-256 |
| --- | --- |
| `cold-copy-ab/summary.json` | `2d3ef5ce8cf9b138df31f1e1887e19a5b2ca02ca12bf55ecc69faa60d8829dcc` |
| `cold-peer-ab/summary.json` | `2d0ad0e77ff52e3cf39daf62a6317f661f68d246593b4790503564695d38b33f` |
| `cold-followup/pack-comparison/summary.json` | `2b6e1902161fb1f10c11e52e6a01bce66b6ebc234a5d153ef762c1e944db8dd2` |
| `cold-followup/nvx-boot/summary.json` | `2c35fc1967ea5a4e3f734dccf4568232b6f72bbfd0edb31e36e695707bf8eec7` |
| `microsandbox-release-recheck/paired/summary.json` | `ef68a9471f0ed67f92149acf563b63766d6d174148dfa8cf3568f68985031ffc` |
| `warm-init-confirm/measurement/summary.json` | `023e1244d9f837d915399bc8edc7585774e97ecb5857472259d986a81891fbd4` |

上游固定来源：
[Microsandbox v0.7.11](https://github.com/superradcompany/microsandbox/releases/tag/v0.7.11)、
[v0.6.10](https://github.com/superradcompany/microsandbox/releases/tag/v0.6.10)、
[smolvm v1.32.4](https://github.com/smol-machines/smolvm/releases/tag/v1.32.4)、
[NVX release](https://github.com/microsoft/nvx/releases/tag/v0.1.0-dev.ffd79a10b65b)。
旧诊断/原型、完整报告和本次整理前的工作树另存
`pr-preparation/working-tree/`，不把当时的实验开关描述成当前支持的产品接口。
