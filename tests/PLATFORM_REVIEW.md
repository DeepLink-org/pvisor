# 跨平台 VM 支持审查

Windows 的 `pvisor.exe` 已能构建并通过 WHP 的共同负载，支持范围为直接 VM
执行。完整 Job、TUI、replay 和 Daemon 仍需移植。提交 `5b320104` 的 macOS CI
在创建 VM 时失败，三平台验收尚未完成。

审查基线为 `a2f6bb81`，重点检查 Windows 后端 `482d7b25`（#84）和 CI 门禁
`705ba52d`（#85）的 CLI 依赖、VM API、WHP builder、virtio-fs、supervisor 与退出
协议。审查未穷尽四万余行 backport，也未覆盖最近全部文档和镜像分发改动。

## 发现与修复

### [P1，已修复] 目录重命名后，缓存子文件仍访问旧路径

`src/windows/devices/virtio/fs/windows/passthrough.rs::rename_inode_path` 原来
只更新目录自身。先 `lookup before/nested/file`，再把 `before → after`，已有
child inode 的 getattr/open 会访问旧路径并可能报 ENOENT。

修复同步更新缓存子树，保留 inode 和 lookup count；同路径 rename 保持缓存。
原生回归检查嵌套缓存 inode，共同 guest 负载检查实际目录移动。原先只重命名单个
文件的 smoke 无法触发这个问题。

### [P1，已补入口与门禁] Windows 构建未覆盖产品 CLI

`pvisor-cli` 无条件依赖 Unix runtime、OverlayFS 和 replay；原 WHP smoke 直接
调用 Rust API。本次增加 `pvisor vm-run`，共用公共 VM API 和 GuestConfig，
Windows 构建排除 Unix 运行时依赖。`pvisor run/spawn` 仍未移植。

Linux/macOS 安装包含 `pvisor`、`pvisor-tui`、`pvisor-replay`、`pvisor-daemon`。
Windows 只构建主程序和测试探针。`pvisor-guest` 是 Linux payload，固件 DLL 是
启动依赖。功能与组件的对应关系见[二进制组成](../docs/src/zh/reference/platforms.md#binaries)。

### [P2，已补覆盖] WHP 缺少并发文件负载

原 `whp_guest` 只有一次读取和写回，WHP 验收共五个场景。新增驱动默认启动
12 次 VM，每次运行 8×32 次文件操作，并检查内存触碰、只读拒写、计时器、SMP
和退出码。宿主还检查结果文件，避免 CLI 返回 0 而 guest 没有执行的假阳性。
超时会失败并保留 stdout/stderr、命令、哈希和 rootfs。原 pause/resume、ready
回调失败清理及退出防伪 smoke 保留。

## 验证结果

[首轮 CI](https://github.com/DeepLink-org/pvisor/actions/runs/41288456086)对应提交
`5b320104`。后续提交需重新检查。

| 环境 | 结果 |
| --- | --- |
| Windows 原生 CI | MSVC 构建/契约、WHP API smoke、共同 CLI 负载通过 |
| Linux/KVM CI | 共同 CLI 负载通过 |
| macOS CI | 四程序构建和独立 CLI 测试通过；共同负载在 guest 启动前报 `build hvf aarch64 VM: Internal(Vm(VmSetup(VmCreate)))`；core shard 的 `opencode_continuation_preserves_failure_and_step_budget` 另有超时 |
| 本机 Apple Silicon | HVF 默认 12/12 次启动及追加 30 轮、120/120 次启动通过；129 项 CLI、14 项 VM API/boundary、4 项 Python 驱动回归通过 |
| Windows 交叉检查 | x86_64 MSVC `cargo check` 通过，已用固定 1.98.0 工具链复核；另用 stable 检查 VM 原生测试的编译 |
| 文档与格式 | 严格双语构建通过，134 对文章、270 个 HTML 页面；workflow lint、格式检查通过 |
| 本机 macOS lint | `cargo clippy ... -D warnings` 被 `pvisor-overlay-core/src/apply.rs:1698–1699` 的既有未使用变量阻断，该文件未改动 |

本机 HVF 通过和 Windows 交叉检查分别说明本机运行与目标编译的结果；macOS
runner 的 VM 创建失败原因仍待确认。该轮主 CI 未全部通过，用例未放宽或跳过。

## 未覆盖的行为

尚未验证小时级稳定性、持续启停后的 handle/线程/内存泄漏、多个 VM 同时运行、
极低内存或 64 CPU、休眠恢复及宿主时钟跳变。Windows 的大小写冲突、长路径、
ACL/reparse point、mmap、SQLite、锁、打开后删除和文件替换还需独立用例。
网络、快照、stage/apply 和模型调用不在这组负载中。

测试固定输入和最终结果，允许线程调度变化。共享树仍要求调用方管理并防止
不可信宿主改写，新增 CLI 没有加强这项边界。后续优先补持续启停的资源基线和
上述文件语义测试；平台实现及验收条件见平台文档。
