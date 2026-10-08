# 0008：Host 与 Guest 控制权限分离 {#adr-0008}

**状态：实现回填。** 记录当前协议与所有权边界，不据此扩大平台验证范围。

## 背景 {#context}

guest 需要报告状态、接收 directive 并参与 checkpoint 静默点；宿主调用方需要管理 Job、VM、stage 和 supervisor。两者的信任范围不同，guest 协作凭据不能获得宿主生命周期权限。

## 备选与选择 {#decision}

可以把宿主操作塞进 Guest `Hello`/`Sync` 协议，也可以分别定义端点、schema 和凭据。当前选择后者：Guest AgentCtl 保留工作负载协作，Host envelope 承载宿主类型化请求，端点所有者核对 Job/Attempt/generation 与权限。

CLI Job listener 在同 UID 的私有权限根下接入请求，通过独立 ticket 与 worker 通道交接。daemon native supervisor 使用 Host envelope 加 owner/token，不借用 Guest token 或 CLI worker ticket。共享 Core 只定义 envelope 与校验；CLI DTO 和传输保留在对应实现。

## 后果 {#consequences}

Bundle 的 `agentctl` 状态仍表示 Guest 协作，不能作为 Host 授权回执。两个协议当前都为 version 1，也不能互换。CLI worker schema、包版本和 executable digest 共同决定内部兼容性；旧 listener/supervisor 需要按合同排空升级。

Host `request_id` 提供关联，不提供所有命令的持久去重。超时或取消后先核对 Job 与效果。私有目录和同 UID peer 检查限定宿主信任边界，不隔离同 UID 的全部其他进程；macOS 身份、后代清理和真实 VM TUI 路径保留当前文档列出的验证缺口。

## 实现与范围 {#implementation}

源码入口：`crates/pvisor-core/src/host_protocol.rs`、`protocol.rs`；`crates/pvisor-cli/src/cli/host_service.rs`、`host_fds.rs`；`crates/pvisor/src/runtime/host_transport.rs`、`supervisor.rs`、`instance_control.rs`。

详细权限与升级合同见[Host/Guest AgentCtl](../architecture.md#host-agentctl)，各版本见[协议矩阵](../records-and-versions.md#control-protocols)，断连与重试见[失败语义](../failure-semantics.md#idempotency)。
