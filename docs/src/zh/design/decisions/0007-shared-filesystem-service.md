# 0007：Host FUSE 与 virtio-fs 共用文件服务 {#adr-0007}

**状态：实现回填。** 记录当前源码中的职责划分，不代表新增正式批准或性能结论。

## 背景 {#context}

Host 暂存通过宿主 FUSE 服务文件，VM 通过 guest virtio-fs 请求文件。分层查找、权限、copy-up、原像与 lazy lower 读取需要保持同一语义；inode、handle、凭据和队列完成则依赖各自协议入口。

## 备选与选择 {#decision}

可在两个适配器中分别维护文件语义，也可让 VM 请求再穿过宿主 FUSE，或直接调用共享文件服务。当前选择第三种：`pvisor-overlay-core::service::FilesystemService` 持有 OverlayCore，Host FUSE 和 VM virtio-fs 直接调用；本地文件与不可变镜像后端在服务中统一读取。

服务接收文件操作、路径和 backing 身份，不接收 `fuser::Reply*`、virtqueue descriptor 或 guest RAM 指针。适配器仍拥有协议 inode/handle、权限转换、operation guard 与完成发布。VM 不需要中间 `/dev/fuse` 挂载。

## 后果 {#consequences}

文件语义只有一个实现，但两个入口仍有平台和并发差异。virtio-fs worker 持有 RAM lease 到 used-ring 发布，冻结需要排空在途 I/O；共享服务的内容读取不应持有整个协议 handle 表锁。lazy lower 的 copy-up 或完整导出仍可能物化此前未读内容，不能由接口复用推断端到端性能收益。

## 实现与范围 {#implementation}

源码入口是 `crates/pvisor-overlay-core/src/service.rs`、`crates/pvisor-overlayfs/src/fs.rs`、`crates/pvisor-vm/src/devices/virtio/fs/` 以及 `crates/pvisor/src/image/cache/backend.rs`、`direct.rs`、`lazy.rs`。当前 `pvisor-overlay-core` 尚未迁移到单一 `api` 入口；复用文件服务与 API 可见性迁移分别管理。

请求路径、数据布局和已有测量范围见[文件系统子系统](../overlayfs.md#filesystem-service)，映射生命周期见[VM 子系统](../vm-runtime.md#virtio)。
