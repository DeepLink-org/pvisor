# libkrun 上游同步与本地定制

2026-10-02。当前为 **1.19.3 基线上的选择性回移植**，不是完整升级到 1.19.6 或 2.0。Cargo 版本、依赖锁和 `.cargo_vcs_info.json` 保留真实原始来源；后续改动由本页逐项登记。

## 来源与比较范围

- 上游：[libkrun/libkrun](https://github.com/libkrun/libkrun)；旧 containers/libkrun 地址已重定向。
- 原始基线：`654e4a6045858d5ced90efae106e91d0eca3469f`，libkrun 1.19.3。
- 稳定分支：`stable-1.19.x` / v1.19.6，`227b2de6ed323fe180e02f871c5f325a90c13cc2`。
- 主线比较快照：`97a914ee06210fa2553b2ab75493bf6e8888910e`。上游说明 main 是 API/ABI 不兼容的 2.0 开发版。

上游完整历史和所需源码拉到临时目录供比较；仓库内只引入下表所选实现。不能把本页所列快照视为未来上游 HEAD，或把上游作者的测试结果视为 pVisor 的运行证据。

## 本次引入

| 来源提交 | 改进 / 特性 | 适配与行为边界 |
|---|---|---|
| [17642fe](https://github.com/libkrun/libkrun/commit/17642fe3aae77ee8089a98c709822ed3559f785d) | Linux / AArch64 SVE | 能力检测后启用并在寄存器配置前 finalize；不支持的主机保持原路径。 |
| [dfc4aa4](https://github.com/libkrun/libkrun/commit/dfc4aa44610e8c57c540c01b0c3e3dfc51e6180c) | Linux / x86_64 PIT | 禁用 missed tick reinjection，保留 PIT 本身；不承诺性能提升幅度。 |
| [e2c05e1](https://github.com/libkrun/libkrun/commit/e2c05e124777a3061a3b23307a6388d042667d52) | 未知 ioctl | Linux passthrough 返回 ENOTTY；本地补齐 FileSystem 默认实现，使 Overlay / macOS / Null 路径一致。AugmentFs 私有退出码 ioctl 保留。 |
| [a47a1ff](https://github.com/libkrun/libkrun/commit/a47a1ffa9479097bbccda42c59f28ac77ea476b6) | macOS capability xattr | 不存在的 security.capability 按 ENOATTR 判断，不产生错误警告。 |
| [7a4a4b9](https://github.com/libkrun/libkrun/commit/7a4a4b9218c632d55392443f3fd5f98939fa4e4e) | macOS 私有 xattr | Linux guest 看不到 com.apple.*；get/remove 返回缺失，set 拒绝，list 的大小及正文同时过滤。宿主属性不删除。 |
| [e77e9dc](https://github.com/libkrun/libkrun/commit/e77e9dc656c0adb3fe047156b0a890c2c82f98c7) | vsock RST 锁 | 释放 proxy_map 读锁后处理 proxy removal，避免读锁升级造成死锁。 |
| [d2d8dd6](https://github.com/libkrun/libkrun/commit/d2d8dd660641abcb8b258aaf244021d366ccd235) | vsock 拒绝连接 | 未建立连接立即回收，避免宿主连接一直等待 reaper。 |
| [f948cfb](https://github.com/libkrun/libkrun/commit/f948cfb9eaf342cd2405379ad7fc8618b1d153b9) | 设备事件循环 | block / fs / net / snd / vsock 的 epoll buffer 在循环外复用；按本地旧版 Epoll 接口适配，不引入新版 krun-utils。 |
| [cf7f38f](https://github.com/libkrun/libkrun/commit/cf7f38f96249af3540f2a443308e03dc619d8bd9) | macOS shutdown eventfd | 创建 context 不再预分配 shutdown device，首次请求才安装；返回独立、调用者持有的 fd。本地使用 F_DUPFD_CLOEXEC，保留其他平台 1.x 符号及错误返回。 |

七项稳定分支补丁直接按 vendored 路径应用；event-loop buffer 与 shutdown eventfd 在本地接口上适配。未知 ioctl 的 FileSystem 默认行为是配合 pVisor Overlay 的额外修正，没有增加通用文件系统抽象。

macOS shutdown fd 的所有权合同有变化：调用者现在应关闭返回的独立 fd，关闭或释放 context 不影响另一方的 fd 所有权。pVisor 当前 runner 不调用此 API，仍沿现有取消与进程监管路径结束 VM；本次没有将新能力接成产品级“优雅关机”承诺。

## 已存在，不重复引入

| 上游提交 | 本地已有实现 |
|---|---|
| [4c9c185](https://github.com/libkrun/libkrun/commit/4c9c185c8ac02b1bb21420834b5651767f0c9cc9) | Linux worker 恢复原有效 uid/gid |
| [e878ca7](https://github.com/libkrun/libkrun/commit/e878ca7f7447f6bceda2aaef1ff653e76b27f064) | 凭据恢复回归检查 |
| [3231515](https://github.com/libkrun/libkrun/commit/3231515be70c12b632142136823310da03b34d67) | virtio-fs used ring 填写真实回复长度 |
| [d96c120](https://github.com/libkrun/libkrun/commit/d96c120e54df9f0b1080e9f8533e260ae1f43ad5) | macOS rename 覆盖目标时保留 inode 的 fd |
| [3d2080e](https://github.com/libkrun/libkrun/commit/3d2080e8199b286942e720d43806699b8845bd5c) | macOS readdir rewind 清空并刷新缓存 |

这些实现存在本地适配，未声称整个文件与对应上游 commit 字节一致。

## 保留的 pVisor 定制

- `vendor/libkrun/build.rs` 构建并嵌入 Rust pvisor-guest；rlib 静态链接与 Linux musl embedded kernel 不变。
- `krun_add_virtiofs_overlay_with_policy`、preimage / excluded paths、共享 OverlayCore、FileAccessPolicy，以及 AugmentFs 退出码 `0x7602` 保留。
- macOS fd 路径后端、guest 权限语义、allow_idmap 开关保留。
- TSI 继续由 pVisor 禁用，网络走受策略控制的 virtio-net / smoltcp；vsock 修复不会启用绕过该数据面的网络。
- 未新增 crate、依赖、固件下载、平台支持或 Cargo feature；不是借同步重写 vendor。

## 暂缓项与理由

### 后续本地适配：VM 原语与文件 RAM backing

在保持 1.19.3 配置 API 和 Overlay 定制的前提下，增加 Rust
`VmmHandle::{pause,resume,offload_ram}` 与 `krun_start_enter_with_handle`，由 pVisor 的
`RunHandle::{pause,resume,offload}` 通过私有 socketpair 调用。不是 2.0 C ABI
升级。参考上述 main 快照的 handle 和 macOS vCPU 控制；本地额外接通
Linux 已有暂停事件，并增加全 vCPU 确认、超时和失败后取消。

新增 `krun_set_ram_backing` 的 FD 配置：普通 RAM 使用共享文件映射，x86_64
kernel bundle 复制到该 backing；DAX/GPU 窗口排除。Linux 写回、丢弃映射页
并请求文件缓存回收；macOS 先解除 HVF RAM 映射，写回/失效缓存，resume 时
恢复映射。新路径通过同文件系统硬链接发布，不复制或替换活跃 RAM。

普通 pause 时设备 I/O 和墙钟继续推进。offload 增加 VM-local RAM 访问门禁，
queue/descriptor、Reader/Writer、vsock packet 和网络 TX 地址引用纳入排空；
排空时不持有 VMM 锁，超时禁止恢复。resume 打开门禁；未覆盖的 GPU/audio/input/TEE
构建拒绝 offload。不包含虚拟时钟冻结或完整快照；实际回收量不作保证。
macOS 的 vtimer offset 与 HVF WFE deadline 修正没有拆开回移植。
**这项后续适配未编译、未运行测试，不能套用下方先前版本的验收结果。**
接口、边界及验证状态见 [VM 控制说明](../../docs/vm-pause-resume.md)。

| 改进 | 本次不引入的原因 / 后续条件 |
|---|---|
| 全量 1.19.6 | arch / hvf / cpuid 等仍用 1.19.3 精确版本；完整升级需成组更新并验收，单改版本号会伪造已同步状态 |
| 2.0、Windows、VMM 合并与新 API | 影响 crate 拆分、guest init 和 pVisor 执行器接口，需要独立迁移 |
| macOS >64 GiB RAM、AMD CPUID 新叶 | 涉及当前未 vendored 的 arch / hvf / cpuid，不能只移植调用端；有明确需求后成组处理 |
| secctx 0444 临时 chmod | 上游方案暂时授予 owner 写权限且恢复失败仅日志；本次不改变宿主权限边界，需先确认更严格的对象及恢复合同 |
| vsock clean shutdown / TCP half-close | 后续主线继续修改 half-close；pVisor 不使用 TSI，避免只引入中间状态机补丁。未来启用相关功能时成组移植并验收 |
| GPU / render server / block ID / 上游 init UID-GID | 当前执行路径无相应消费；上游 init 用户解析不能代替 pVisor guest 的 OCI 用户支持 |

## 验证与未验证范围

回移植阶段仅做静态检查；2026-10-02 用户随后明确授权编译与测试，已完成新一轮运行验收。

- `just build debug`：macOS arm64 构建、签名验证通过。
- 产品 Rust：935 通过、5 个宿主挂载相关失败、4 跳过；Python：56 通过、19 跳过。
- libkrun / krun-devices 专项：57 通过，包含 ioctl、macOS xattr 与 shutdown fd 回归。
- Linux x86_64 krun-devices、Linux AArch64 krun-vmm / krun-devices：musl 交叉编译检查通过，未运行 KVM。
- 真实 HVF VM：基础 FS、退出码、smoltcp 网卡初始化通过；硬链接身份及缓存一致性失败。相关 Overlay inode 实现未被本次回移植修改，但未运行升级前 VM 对照。

**验收未全绿，不能声明没有回归。** 完整命令、已修复的产品问题、FSKit 环境失败与硬链接复现见 [编译与运行验收报告](../../review_project/06-evidence/libkrun-validation.md)。未验证 Linux SVE / PIT 运行、vsock 压力与真实关机；缓冲复用未测性能。
