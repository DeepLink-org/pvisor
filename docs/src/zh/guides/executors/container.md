# 原生 OCI 容器

container 执行器在 Linux 宿主内核上运行 OCI 镜像的用户空间。它直接调用 `runc` 或 `crun`，不需要 Docker 或 Podman。

```bash
pvisor run --executor container \
  --container-image ubuntu:24.04 \
  --stage ../stage-container -- /bin/sh
```

`--container-image IMAGE` 会自动选择 container 执行器；`--executor container` 让选择显式。`--container-rootfs PATH` 可以直接使用已有 rootfs，否则 pVisor 用自带的 OCI image store 准备镜像。

`--container-platform linux/amd64` 或 `linux/arm64` 是可选的原生架构断言，不是跨架构执行或下载选择器。与宿主匹配的断言会被接受；不匹配时在启动前拒绝，即使设置了 `--container-rootfs`。host 和 VM 配置会拒绝此选项，不会静默忽略。TOML 使用 `container.platform = "linux-amd64"` 或 `"linux-arm64"`。

注入程序默认是当前运行的 pVisor。需要兼容的 Linux 构建时才指定 `--container-pvisor-binary PATH`；pVisor 不会根据平台断言自动发现或下载另一个程序。rootfs 和二进制须由你准备，并兼容原生架构及 guest ABI。下方示例假设 Linux x86_64。参见 [CLI 参考](../../reference/cli.md)、[配置字段](../../reference/config.md#settings)与[容器场景](../../reference/cases.md)。

## 工作方式

pVisor 生成标准 OCI bundle，把当前或显式提供的兼容 Linux pVisor 二进制挂进 rootfs，再在容器内走普通的 `pvisor run --executor host --spec ...` 路径。Agent 命令放在 RunSpec 中，不暴露在 OCI runner 的 argv 里；容器内的 pVisor 创建自己的 AgentCtl 并返回类型化结果。

```bash
pvisor run \
  --container-image example/codex-agent:latest \
  --container-pvisor-binary ./dist/pvisor-linux-amd64 \
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

- container 执行器记录容器隔离，但**不声称完整的能力强制**；
- 因此 `--safe` 在 container 上会拒绝启动，而不是给出一个不完整的边界；
- 使用 host 网络时，选择性网络策略与 host 一样是协作式的。

需要独立内核或不可绕过的网络边界时，改用 [VM](vm.md)。
