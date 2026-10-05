# 已有 Docker 或 devcontainer，还需要 pVisor 吗？

## 主要结论 {#conclusions}

**已有 Docker + worktree/Git 工作流满足需求时可继续使用。pVisor staged 提供保留改动和选择性合入，但增加文件访问成本。所测 Docker VFS 创建成本不代表 overlay2 或 Docker Desktop。**

| 需求 | 选型含义 |
|---|---|
| 已有 Docker + worktree/Git | 保留既有流程，按实测评估工具成本 |
| 跨 Agent 统一暂存与选择性合入 | 评估 pVisor host staged |
| 要求独立 guest kernel | 对比 VM 执行成本 |

## Motivation {#motivation}

容器提供工具环境，但工作区怎样挂载决定改动是否即时抵达宿主。比较速度时，审查、导出、冲突处理和合入所需的流程也影响最终成本。

## 实验设计 {#interpretation}

共享 Linux/x86_64 宿主，AMD Ryzen 7 9700X，Fedora 内核 7.2.8-200.fc44.x86_64。启动进程树及专用 Docker daemon 固定到宿主 CPU 0,1；guest 为 2 vCPU。host/staged 使用 rootless_process。Shell VM 为 128 MiB，工具 VM 为 16 GiB。原生/Docker 不限内存：控制 CPU 与 guest 配置内存，不是相同资源限制的对照。工具与输入已准备，每次新建工作区、热缓存，3 次预热、60 次正式采样；按固定种子随机交错后端。构建、下载及输入复制不计时。

Docker Engine 29.7.2 使用专用 rootless VFS daemon 与可写 bind mount，结果不代表 overlay2 或 Docker Desktop。Firecracker 1.13.1 PCI 不使用 jailer；QEMU 10.2.2 分别使用 q35/microvm、私有 ext4。pVisor VM 使用 virtio-fs 和不同内核。内核、存储、设备及暂存语义均有差异，不能把差距单独归因于 VMM 或 FUSE。

均使用新建、经校验的 fixture。单项不含启动/退出，完整任务包含二者。devcontainer 插件、overlay2、Docker Desktop 与对等 Git 审查耗时未测。

## 实验数据和分析 {#results}

### 本机任务与文件 {#reference-comparison}

启动/文件系统：2026-10-05；修复：2026-10-06。独立负载各 N=60、失败 0、3 次预热。通常为 P50，分离簇展示中位数与数量；不跨负载合并。

| Operation | Unit | Native | pVisor staged | Docker rootless / VFS | pVisor VM |
|---|---|---|---|---|---|
| 首条输出 | ms | 1.24 | 24.99 | 3380.93 | 99.76 |
| 修复到退出 | s | 0.45 | 0.68 | 5.15 | 3.25 |
| 七项工具到退出 | s | 0.51 | 1.29 | 5.68 | 6.66 |
| 遍历 2,048 文件 | ms | 4.68 | 74.90 | 5.03 | 227.11 |
| 读校验 64 MiB | ms | 33.28 | 68.85 | 32.48 | 154.80 |
| 离线 npm 安装 | ms | 190.34 | 263.19 | 273.68 | 1662.23 |

Bind mount 工具访问可以较快，而 VFS 创建较慢。复用容器可摊薄创建成本，一次性环境仍须支付。staged 原文件直到 apply 才修改。

### 改动工作流

| 配置 | 改动/审查工作流 |
|---|---|
| Docker 可写 bind | 直接改宿主文件；可另加独立 worktree |
| Docker layer / volume | 通过导出、patch 或 commit 合入 |
| devcontainer | 按配置挂载/volume，配合 Git/PR 审查 |
| pVisor staged | 保留改动、路径选择、preimage 冲突校验 |

官方 [Docker bind mount 文档](https://docs.docker.com/engine/storage/bind-mounts/)说明默认宿主写入；[devcontainer 规范](https://containers.dev/)说明配置。除工具速度，还应比较[隔离](isolation-tests.md)与[apply](apply.md)。固定版本 [Agent CLI 测试](agent-tasks.md#cli-compatibility)使用受控响应，不排名默认内置 sandbox。

### 数据下载与复现 {#run}

[整理后的表格 CSV](compare-containers.csv) · [运行时统计](runtime-summary.csv) · [来源与制品](runtime-provenance.csv) · [证据来源摘要](evidence-sources.csv) · [比较方法](methodology.md) · [复现手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)
