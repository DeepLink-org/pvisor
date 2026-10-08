# TUI 中查看文件与网络拦截

运行 `run.sh` 会创建演示工作区，再以如下形式启动 Bash：

```bash
pvisor --tui --stage "$stage" \
  --access 'private/**:deny' \
  --overlaynet-deny blocked.example \
  -- bash ./agent.sh
```

Agent 脚本依次：

1. `touch created-by-agent.txt`，在暂存工作区创建文件；
2. `touch private/token`，触发 `--access 'private/**:deny'`；
3. 用 `curl` 经 pVisor 注入的代理访问 `blocked.example`，触发 `--overlaynet-deny blocked.example`。

脚本完成后会进入交互式 Bash，Job 保持运行。按 `Ctrl-]` 再按 `f` 打开 Files 面板，查看路径、操作与拒绝次数；按 `Ctrl-]` 再按 `n` 打开 Network 面板，查看 `HTTP blocked.example:80` 的拒绝记录。按 `Esc` 关闭面板，输入 `exit` 结束 Job。退出 TUI 后，`run.sh` 会显示 `status --review` 的持久证据，并检查 lower 中的受保护文件和暂存文件状态。

```bash
just build release
./examples/pvisor/06-tui-interception/run.sh
```

需要 Linux rootless namespace、FUSE、`curl` 和交互式终端。无终端的回归使用同一脚本的 `--once` 模式：

```bash
./examples/pvisor/06-tui-interception/test.sh
```

host 上的 OverlayNet 代理是协作式的；此示例证明**经过代理**的请求被拒绝，不代表所有直接 socket 访问都被拦截。
