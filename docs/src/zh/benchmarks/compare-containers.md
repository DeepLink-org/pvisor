# 对比：Docker / devcontainer

Docker 加 Git 能组成很好的开发环境。pVisor 补充的是运行期间保留改动、apply 前检查原工作区、选择性合入，以及记录实际限制。已有可靠的 worktree/patch 审查流水线时，是否引入 pVisor，取决于这些流程是否值得统一。

## 比较范围

2026-10-04 核对官方文档。本机 Docker daemon 不可访问，第一版同机容器数据使用 rootless Podman + crun；这是 OCI 对照，不能标为 Docker 实测。固定镜像 ID、rootfs 摘要、版本及参数见[文件系统报告](filesystem.md)。macOS 启动历史和 Linux VM 启动保留在[VM 启动时间](startup.md)。

| 配置 | 文件修改在哪里发生 | 合入时的保护 | 适用场景 |
|---|---|---|---|
| Docker + writable bind mount | 宿主挂载文件即时变化 | 需要另外组织审查和回滚 | 可信开发任务，环境可复现即可 |
| Docker writable layer / 独立卷 | 容器层或卷内 | 导出文件、补丁或提交后自行合并 | 工作区本来就远程或独立 |
| devcontainer | 按配置选择挂载、卷和工具链 | 可与 Git、worktree、PR 流程组合 | 团队标准开发环境 |
| Docker + 独立 worktree + git diff/apply | 独立 worktree，宿主原目录可保持不变 | Git 可做 patch 检查、三方合并；调用方负责工作区和重试协议 | 已有成熟 Git 审查流程 |
| pVisor staged Job | 上层 stage；宿主原目录在 apply 前保留 | preimage 冲突检测、按路径 apply/drop、事务恢复；见实测 | 多 Agent 或非 Git 目录需要统一审查协议 |

Docker 挂载与容器边界依据 [bind mounts](https://docs.docker.com/engine/storage/bind-mounts/) 和 [Engine security](https://docs.docker.com/engine/security/)；devcontainer 的定位依据[开放规范](https://containers.dev/)。Git 工作流仍应检查未跟踪文件、二进制、权限、符号链接及并行修改，不能只查看默认 `git diff` 的输出。

## 性能与实际差别

[文件系统测量](filesystem.md)使用相同输入与工具，对比原生、Podman、pVisor OCI 和各文件视图；镜像准备不计入任务耗时。[apply/drop](apply.md)单独测文件数量增长与冲突拒绝。容器后端存在不表示每种 mount 都进入 stage；确认 Run Bundle 中的暂存范围。

“Docker 加 git diff 够不够”的答案是：已有完整审查/合并协议时可以够。只增加 `git diff` 不能自动撤回已写入 bind mount 的修改，也不会生成 pVisor 的执行能力观察记录。pVisor 的收益来自这些工作流，代价是相应的进程、暂存和记录开销。

## 更正

通过 [pVisor issues](https://github.com/DeepLink-org/pvisor/issues) 提供配置、镜像摘要及命令；欢迎补充真正同机的 Docker/overlay2 样本，本版未将 Podman 数字外推到 Docker Desktop。
