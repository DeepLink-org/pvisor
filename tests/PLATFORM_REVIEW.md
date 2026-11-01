# 跨平台 VM 支持审查

审查基线：`a2f6bb81`，重点为 Windows 后端引入 `482d7b25`（#84）及
主 CI 门禁 `705ba52d`（#85）。追踪 CLI 入口/依赖、公开 VM API、WHP builder、
virtio-fs 路径与 inode、guest supervisor/退出协议和三平台 CI。此次不是对新增的
四万余行 Windows backport 的穷尽审计，也不是最近所有文档/镜像分发改动的完整审查。

## 发现与处理

1. **[P1，已修复] Windows 目录重命名留下失效的子 inode 路径。**
   `src/windows/devices/virtio/fs/windows/passthrough.rs::rename_inode_path`
   原来只更新被重命名目录自身。在 `lookup before/nested/file` 后重命名
   `before → after`，后续对已有 child inode 的 getattr/open 仍访问旧路径，
   可报 ENOENT；通过编译或只创建/重命名单个文件的 smoke 无法发现。
   修复对缓存子树同步更新路径，保留 inode 和 lookup count；同路径 rename 保持
   缓存不变。新增原生私有回归和共享 guest 可观察用例。原生回归必须由 Windows
   CI 实际运行，本机 macOS 的通过不能替代它。

2. **[P1，已补入口与门禁] Windows 的“能编译”原来只覆盖 VM crate，不覆盖产品 CLI。**
   `pvisor-cli` 原来无条件依赖 Unix runtime/OverlayFS/replay，WHP smoke 直接用
   Rust API。不能据此声称 `pvisor run/spawn` 可在 Windows 工作。本次依照明确的
   最小移植范围增加 `pvisor vm-run`：公共 VM API + 同一份 GuestConfig，Windows
   编译排除 Unix 运行时依赖；不移植 Job/container/TUI/replay。三平台 CI 现在均需
   通过实际 pvisor 程序执行相同 guest 负载和结果检查。

3. **[P2，已补覆盖] 原 WHP 验收只有五个场景，缺少并发文件正确性负载。**
   原 `whp_guest` 的文件用例仅一次读取/写回。新增统一驱动默认 12 次 VM 启动、
   每次 8×32 次文件操作、内存触碰、只读检查、计时器、SMP 和精确退出码验证。
   驱动检查宿主文件结果，防止 guest 没执行但 CLI 恰好返回 0 的假阳性；超时失败，
   保留 stdout/stderr、命令、哈希、结果和 rootfs。原 WHP pause/resume 与退出防伪
   smoke 继续保留，不降低原覆盖。

## 验证与仍需的证据

本机 Apple Silicon macOS：默认共同负载 12/12 真实 HVF 启动通过，
追加 30 轮（120/120 次启动）也通过；CLI 的 129 项
单元测试、VM API/boundary 的 14 项测试、驱动的 4 项 Python 回归通过。
Windows CLI 的 x86_64 MSVC 交叉 `cargo check` 通过；已使用项目固定
1.98.0 工具链复核；另外也用本机 stable 工具链编译检查 Windows VM 原生测试。交叉检查不是链接、
原生执行或 WHP 验收。三平台 CI 接线已加入。首轮远端结果见下文。
双语严格文档构建（134 对文章、270 个 HTML 页面）、workflow lint 和格式检查通过。
macOS 上 `cargo clippy ... -D warnings` 被既有
`pvisor-overlay-core/src/apply.rs:1698–1699` 的两个未使用变量阻断；本次未修改该文件。

该负载刻意固定内容与最终结果，线程调度不固定。它没有验证长期泄漏/运行数小时的
稳定性、多个 VM 同时运行、极低内存/64 CPU、休眠恢复/宿主时钟跳变、Windows
大小写冲突/长路径/ACL/reparse point、mmap/SQLite/锁/打开后删除的全部 POSIX
语义，也不验证网络、快照、stage/apply 或模型调用。不要将这组测试外推为这些
能力的支持声明。Windows 普通共享树仍要求调用方管理、避免不可信宿主改写；
这项限制沿用 VM README，没有因为新增 CLI 而获得加强。

下一次扩展应以实际业务负载和故障日志选择回归，优先补 Windows 持续启停时的
进程 handle/线程/内存基线，以及文件替换、打开后删除、大小写和 ACL 的明确契约。
保持结果等价断言，不用放宽错误检查或按平台跳过来获得绿色 CI。

## PR #92 首轮原生 CI 与二进制范围

提交 `5b320104` 的 [CI](https://github.com/DeepLink-org/pvisor/actions/runs/41288456086)：
Windows MSVC 构建/原生契约与 WHP 实际运行（含共同 CLI 负载）通过；Linux/KVM
共同负载通过。macOS 四程序产品构建和独立 CLI 测试通过，共同 VM 负载失败，
保留的 stderr 为 `build hvf aarch64 VM: Internal(Vm(VmSetup(VmCreate)))`，
失败发生在 guest 启动前，不能当作 guest 行为差异或已通过。该轮 macOS core
shard 另有 `opencode_continuation_preserves_failure_and_step_budget` 测试超时。
主 CI 尚不满足全部通过的条件；没有放宽检查或跳过 VM 断言。

Windows CI 只构建限定的 `pvisor.exe` 和测试探针；Linux/macOS 产品安装包含
`pvisor`、`pvisor-tui`、`pvisor-replay`、`pvisor-daemon` 四个宿主程序。前者不能
代表后者的完整功能。`pvisor-guest` 是 Linux guest payload，固件 DLL 是启动依赖，
都不是补齐 TUI/Daemon 的 Windows companion。平台文档的
[二进制组成](../docs/src/zh/reference/platforms.md#binaries)列出按功能选择组件的表格，
以及 IPC、终端、权限、进程归属和安装交付的实现差异及后续验收要求。
