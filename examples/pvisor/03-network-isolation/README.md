# 1.3 pVisor 网络边界

**问题：pVisor 当前控制的是哪些网络流量？可复现结论：OverlayNet 控制 cooperative proxy 流量，不是 network sandbox；direct socket 可绕过代理。**

```bash
./run.sh
mise run examples -- 03-network-isolation  # 从仓库根执行并验证
```

脚本启动一个本地 HTTP server，然后平铺执行三个场景：

1. `--overlaynet-allow` 允许经过代理访问声明的目标；
2. `--overlaynet proxy --overlaynet-deny 127.0.0.1:<ephemeral-port>` 拒绝经过代理访问该目标；
3. `curl --noproxy "*"` 直接连接同一目标，证明 direct socket 可以绕过代理。

核心命令就是普通的 pVisor CLI：

```bash
pvisor stage --executor host --overlaynet-allow 127.0.0.1:<ephemeral-port> -- \
  agent-command

pvisor stage --executor host --overlaynet proxy --overlaynet-deny 127.0.0.1:<ephemeral-port> -- \
  agent-command
```

纯 OverlayNet 运行会将当前目录作为 Run 的项目关联路径；每次执行的 Run 记录和
Bundle 则独立保存在 `PVISOR_RUN_HOME` 的实例 Attempt 中。这里使用持久 `stage`，
以便运行后检查 Bundle；临时 `spawn` 在退出后会清理记录。

`run.sh` 中的短 `bash -c` 只负责让 curl 显式读取 pVisor 注入的 `$HTTP_PROXY`。
它把 allow、deny 和 direct 三次执行的 stdout、stderr 与退出码保存在工作目录中，便于
直接观察。`mise run examples -- 03-network-isolation` 再检查响应、预期失败和三个 Run Bundle。

预期结论：

```text
Conclusion: OverlayNet controls cooperative proxy traffic; it is not a network sandbox.
```

## 安全边界

当前 OverlayNet 是显式 HTTP/HTTPS proxy，不是透明 network namespace：

- 遵守 `HTTP_PROXY` / `HTTPS_PROXY` 的客户端会经过策略；
- 删除代理设置、配置 `NO_PROXY` 或直接创建 socket 可以绕过；
- DNS/UDP 不在当前驱动覆盖范围内；
- Run Bundle 的 `network_non_bypassable` 因此为 `false`。

这里显式选择代理 deny 规则，不使用 `--overlaynet-deny-all`：
后者还会独立请求 Host deny-all 网络隔离，在支持 namespace 的 Linux 上会同时
阻断 direct socket，不能拿来单独演示 cooperative proxy 的边界。

需要不可绕过的网络隔离时，应使用 container/KVM 网络边界，而不是把 cooperative proxy
的 allowlist 或 deny-all 当作安全沙箱。

## Links

- [pVisor examples](../README.md)
- [Network control](../../../docs/src/zh/guides/policies/network.md)
- [OverlayNet architecture](../../../docs/src/zh/design/overlaynet.md)
