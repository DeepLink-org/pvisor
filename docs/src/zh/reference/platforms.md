---
status: todo
search:
  exclude: true
---

# 平台与执行器支持矩阵

!!! warning "规划中"
    成熟度等级仍待研发给出证据。各执行器的边界见[执行器边界](../security/executor-boundaries.md)。

## 要回答的问题

在哪个平台上、用哪个执行器、哪个能力维度，当前处于什么成熟度？

## 需求

- 矩阵：平台（Linux x86_64、Linux arm64、macOS Apple Silicon）× 执行器（host、container、VM）× 能力维度；
- 每格的成熟度等级：稳定、Beta、实验、不支持；
- 每个等级必须附证据链接：CI 任务、语义规格、基准或 issue，不凭空标注。

## 验收标准

- 每格都有等级和证据；
- README 的成熟度标识指向这份矩阵。

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：[执行器边界](../security/executor-boundaries.md)、[已知限制](../security/known-limitations.md)

## 当前机制与准备条件

这张表描述代码路径与发布准备条件，不替尚未完成的验证宣布 Stable/Beta 等级。

| 平台 | host | container | VM |
| --- | --- | --- | --- |
| Linux x86_64 | 暂存需要 FUSE；sandbox 需要可用 user namespace 与 Landlock | 需要原生 `crun`/`runc` 与匹配的静态 Linux 二进制 | 需要 `/dev/kvm`；musl 发布产物内嵌 guest 内核 |
| Linux arm64 | 有源码路径；先核对构建与测试证据 | 需要匹配 `linux/arm64` 的注入二进制 | 需要该宿主/构建的 KVM 支持及匹配 rootfs；不从 x86_64 wheel 推导支持 |
| macOS Apple Silicon | 暂存需要 macFUSE 5.4.0+ 的 FSKit；文件/网络使用 Seatbelt | 原生 OCI Linux 容器路径不支持 | HVF、Hypervisor 签名与 Linux arm64 rootfs/image；不需要 macFUSE |
| Windows / macOS Intel | 当前发布说明不承诺支持 | 不承诺 | 不承诺 |

发布 wheel 当前面向 Linux x86_64 与 macOS arm64；有源码分支不等于有该架构的发布 wheel 或成熟度保证。先运行 `pvisor --version` 和小任务，再核对 Bundle，安装步骤见[安装指南](../start/installation.md)。

## 能力证据怎么验证

- 文件读写分别检查 `safety.filesystem_read_non_bypassable` 与 `filesystem_write_non_bypassable`，不要只看是否 staged。
- 网络检查 `safety.network_non_bypassable` 与 driver；Linux host 选择性代理、container host 网络不能因 executor 名称被当成强制边界。
- container 目前不提供 safe 所需的完整控制，`--safe` 会拒绝；所有当前执行器在 `--strict` 下因 Subprocess 缺口拒绝。
- 资源查看 `resources.effective` 与 `limitations`；macOS 不强制 RLIMIT_AS，普通 host rlimit 不是整棵进程树的总额度。

验证入口：`.github/workflows/ci.yml`、`tests/semantics/stage-apply.md`、`crates/pvisor/tests/` 与 `tests/test_vm_*`。尚未验证的矩阵格不作成熟度结论；安全范围见[执行器边界](../security/executor-boundaries.md)。
