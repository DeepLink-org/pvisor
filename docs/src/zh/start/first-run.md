# 第一次运行

这个示例不需要 Agent 账号或 API Key。一个脚本扮演"假 Agent"：改源码、删文件、写一个额外文件、试图读项目内的敏感路径、试图访问外网；随后你审查结果，只合入想要的改动。请先完成[安装](installation.md)，包括宿主机暂存所需的 FUSE/macFUSE。

## 1. 准备演示项目

```bash
mkdir -p pvisor-demo/project/src pvisor-demo/project/.ssh
cd pvisor-demo/project
printf 'print("v1")\n' > src/app.py
printf 'legacy\n' > src/legacy.txt
printf 'FAKE-KEY\n' > .ssh/id_rsa
git init -q && git add -A && git commit -qm init
```

`.ssh/id_rsa` 只是项目里的占位文件，用来演示暂存视图内的敏感路径被拒；本示例不读写你真实的 `~/.ssh`。

## 2. 写一个"假 Agent"

```bash
cat > ../agent.sh <<'SH'
#!/usr/bin/env bash
set -u
printf 'print("patched")\n' >> src/app.py     # 修改源码
rm -f src/legacy.txt                          # 删除文件
printf 'scratch\n' > scratch.txt              # 工作区根目录的额外改动
cat .ssh/id_rsa >/dev/null 2>&1 || echo 'read .ssh/id_rsa: denied'
curl -sS --max-time 5 https://example.com >/dev/null 2>&1 || echo 'reach example.com: blocked'
SH
chmod +x ../agent.sh
```

## 3. 让它在边界内全自动执行

```bash
pvisor run --safe --overlaynet-deny-all --stage ../stage-001 -- ../agent.sh
```

`--safe` 要求落实文件与网络隔离，并按预设拒绝 `.ssh`、`.gnupg` 与私钥文件等敏感路径；它不选择执行器，也不会静默回退到无隔离的 host 进程。`--overlaynet-deny-all` 独立安装强制网络边界。暂存目录放在项目外，每次运行使用新目录。

## 4. 审查：改动与拦截都在记录里

这次 Job 的存储就是暂存目录，用路径定位它：

```bash
pvisor status --review ../stage-001
```

**File access observations** 列出到达 OverlayFS 的路径与结果：`src/app.py` 的写入、`src/legacy.txt` 的删除、`scratch.txt` 的写入，以及 `.ssh/id_rsa` 读取被拒（`denied=1`）。**Network access observations** 列出到达 OverlayNet 的目标与结果：`example.com` 被拒。

需要区分的是：普通 host 上的选择性代理是协作式的——只有经过代理的请求会被记录，未出现的目标不等于从未访问。强制网络边界由 `--overlaynet-deny-all`、容器离线模式或 VM 提供，见[网络控制](../guides/policies/network.md)。

## 5. 只合入你要的部分

```bash
pvisor apply ../stage-001 --path src
pvisor drop ../stage-001
git status --short
```

`apply --path src` 只把 `src` 下的改动写入项目，`drop` 丢弃剩余的 `scratch.txt`。若你在 Run 期间自己改了同一个文件，`apply` 会拒绝覆盖，而不会静默合并。

## 6. 换成你的命令

```bash
pvisor run --safe --stage ../agent-stage-001 -- codex
pvisor status --review ../agent-stage-001
```

已安装的 Agent CLI 使用同一个入口。用 `--stage PATH` 时，后续命令请把该路径或命令输出的 Job ID（`run-*`）显式传进去；`last` 只在默认存储中解析，跨项目并行时不要依赖它。
