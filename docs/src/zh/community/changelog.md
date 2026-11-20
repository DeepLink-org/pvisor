# 发布说明

## pVisor 0.4.0 {#v040}

用 `pvisor run` 在独立 VM 中运行容器，或用 `pvisor spawn` 运行任务 Session 并审查暂存改动。
Linux x86_64 和 Apple Silicon macOS 提供稳定版 wheel 与原生归档。Windows x86_64
可从源码构建实验性的 WHP 容器 profile。[平台要求](../reference/platforms.md) · [安装 pVisor](../start/installation.md)

| 工作流 | 可用能力 |
| --- | --- |
| 容器与 Compose | 在 Unix 上用 Docker 风格命令管理容器型 Session。实验性的 `pvisor compose` 无需 daemon，可按项目启动服务、检查依赖和清理资源。[Compose](../reference/cli.md#compose) |
| 镜像分发 | OCI 拉取可向仓库的 Bearer token 端点认证。用 `pvisor push` 发布懒加载镜像，用 `pvisor image pull --from` 导入固定来源。[镜像后端](../reference/shared-image-cache.md) |
| Agent 任务 | 在 DSH 中监控任务，为符合条件的运行中 Linux Host Session 连接终端。适配器在 `auto` 模式优先使用 VM，并报告选中的执行器。[Agent 插件](../guides/agents/plugins.md) |
| VM 复用与回收 | 符合条件的 Linux/KVM 容器可在同一宿主机进行写时复制分叉。可为子分支启用 offload/resume，或把普通容器 VM 登记到 Linux 内存压力服务，由调用方管理安全等待点。[VM 生命周期](../guides/infrastructure/lifecycle.md#container-memory) |
| containerd | 为 VM 设置节点级默认出口策略，并按需在 Pod 网络命名空间内转发符合条件的 guest TCP 监听端口。[Shim 接入](../guides/infrastructure/integration.md#containerd) |
| 文件结果 | apply 返回失败和恢复证据。工作区快照排除持有租约的同级暂存区。[审查并应用](../guides/review-apply.md) |

Replay 将资源采样与轨迹调度分开执行。发布门禁核对验收记录、待发布 wheel 和源码提交。
详见[发布流程](releasing.md)与[分特性测试](testing.md)。
