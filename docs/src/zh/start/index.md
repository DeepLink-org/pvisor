# 从这里开始

**PolicyVisor（pVisor）** 让 Agent 全自动执行，文件改动由你决定去留。它运行你现有的 Agent CLI、脚本和自动化命令，并把每次运行实际生效的限制记录下来。

Python 包和 CLI 统一使用 `pvisor`。先按[安装指南](installation.md)安装并确认平台要求；暂存的 host Job 需要 FUSE/macFUSE，普通 host Job 默认直接写入工作区。

## 完成第一个闭环

在一个可丢弃且还没有 `result.txt` 的测试项目目录中，用离线命令验证暂存和审查，不需要 Agent 账号或 API Key：

```bash
pvisor run --safe --overlaynet-deny-all -- /bin/sh -c 'printf "candidate\n" > result.txt'
pvisor status --review last
pvisor inspect last -- cat result.txt
```

命令应返回零，原项目中还没有新增的 `result.txt`。Job 停止后，`inspect` 在内核只读的 OverlayFS 工作区视图中运行宿主的 `cat`，应读到 `candidate`。审查结束状态、实际控制和文件改动后，选择保留或丢弃。

保留文件，执行：

```bash
pvisor apply last --path result.txt
```

或者丢弃候选，执行：

```bash
pvisor drop last
```

这个示例只有一个改动，apply 后即完成并清理一次性暂存数据，无需再 drop。有多个改动且分批 apply 时，未选中的候选才继续保留，可随后 apply 或 drop。完整的[第一次运行](first-run.md)还演示文件删除和访问拦截。

## 换成实际任务

把 `--` 后的命令换成已安装的脚本或 Agent CLI，并显式配置所需凭据和网络目标。

需要指定存储位置时，用项目外的新目录 `--stage PATH`，后续命令用该路径或输出的 Job ID，不能依赖只查默认存储的 `last`。HOME、VM 根目录与工作区的写入去向见[暂存与存储](../reference/cli.md#暂存与存储)。远程 API、数据库写入和消息不在文件暂存范围内。
