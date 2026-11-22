# 原生 OCI 容器

container 执行器在 Linux 宿主内核上运行 OCI 镜像的用户空间。它直接调用 `runc` 或 `crun`，不需要 Docker 或 Podman。

```bash
pvisor spawn --executor container \
  --container-image ubuntu:24.04 \
  --stage ../stage-container -- /bin/sh
```

`--container-image IMAGE` 会自动选择 container 执行器；`--executor container` 让选择显式。`--container-rootfs PATH` 可以直接使用已有 rootfs，否则 pVisor 用自带的 OCI image store 准备镜像。

`--container-platform linux/amd64` 或 `linux/arm64` 是可选的原生架构断言，不是跨架构执行或下载选择器。与宿主匹配的断言会被接受；不匹配时在启动前拒绝，即使设置了 `--container-rootfs`。host 和 VM 配置会拒绝此选项，不会静默忽略。TOML 使用 `container.platform = "linux-amd64"` 或 `"linux-arm64"`。

安装默认运行时 `runc`，或显式选择 `crun`。下方示例假设 Linux x86_64。参见 [CLI 参考](../../reference/cli.md)、[配置字段](../../reference/config.md#settings)与[容器场景](../../reference/cases.md)。

## 工作方式

pVisor 在宿主准备工作区和代理，并把任务参数写入 OCI bundle。OCI 运行时直接启动任务。超时、输出、结果和可供审查的暂存改动都由同一个宿主 Session 管理。

```bash
pvisor spawn \
  --container-image example/codex-agent:latest \
  --container-platform linux/amd64 \
  --container-network none \
  --container-mount \
    'source="/host/cache", target="/cache", read_only=false' \
  -- codex
```

## 网络模式

| 模式 | 用途 | 限制 |
| --- | --- | --- |
| `--container-network host` | 使用 Gateway 或显式 OverlayNet 代理 | 必需：注入的代理地址是宿主 loopback |
| `--container-network none` | 离线运行 | 不能同时使用需要 host 网络的代理或 Gateway |
| `bridge` | — | 需要外部 CNI 配置，当前会被拒绝 |

## 已知缺口

- container 执行器记录容器隔离，**缺少完整的能力强制证据**；
- `--safe` 在 container 上拒绝启动；
- 使用 host 网络时，选择性网络策略与 host 一样是协作式的。

需要独立内核或不可绕过的网络边界时，改用 [VM](vm.md)。
