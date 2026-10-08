# 加固建议

按风险从低到高，逐级收紧。每一级都可以用 `pvisor status --review` 核对实际安装的控制。

## 第一级：保护工作区

```bash
pvisor run --safe -- codex
```

- 工作区改动进入暂存区，审查后再 apply，冲突时拒绝覆盖你的修改；
- 视图内的 `.ssh`、`.gnupg` 与私钥文件被拒绝，`.env`、`*.pem` 等访问会告警；
- HOME 使用独立视图，凭据默认不传入。

## 第二级：使用强制网络边界

协作式代理只约束遵守代理设置的客户端。需要强制边界时：

```bash
# 只允许必要的目标（VM 上不可绕过）
pvisor run --safe --vm --rootfs image=my-agent-image:latest \
  --overlaynet-allow api.openai.com:443 -- codex

# 或者在 host 上拒绝所有普通出口
pvisor run --safe --overlaynet-deny-all -- ./agent.sh
```

macOS host 的 `--safe` 本身已阻断直接外部连接；Linux host 的选择性规则仍是协作式的。各路径的差别见[网络边界](../guides/policies/network.md#网络边界)。

## 第三级：收紧凭据

- 不要把长期凭据通过 `--pass-env` 交给 Agent；优先配置 Gateway，由可信侧持有上游 Key，见[凭据与环境变量](../guides/policies/credentials.md)；
- 为项目里自定义命名的秘密文件追加规则，例如 `--access 'config/secrets/**:deny'`；预设只认常见文件名，不识别改名副本或源码中的密钥；
- 不要用 `--mount SOURCE:write` 共享含凭据的目录，显式共享不经过工作区的文件规则。

## 第四级：缩小可见范围

- 在 Linux 上用 `--filesystem sandbox` 把读取限制在投影根目录内；
- VM 上用 OCI 镜像而不是 `--rootfs host`，只通过 `--mount` 暴露需要的路径；
- 固定镜像摘要、Agent 版本与模型版本，保留 Run Bundle 以便事后核对。

## 验证失败关闭

`--strict` 要求每个请求的能力维度都有不可绕过的强制证据，缺少时在 Agent 启动前拒绝运行。当前没有执行器声称完整的子进程强制，所以它会拒绝运行；用它来确认你的流水线在边界不足时会停下，而不是当作更强的沙箱。
