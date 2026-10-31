# 任意脚本与自动化命令

运行一个离线脚本，检查它生成的报告，再只接收报告、丢弃临时笔记。不需要模型账号或第三方依赖；先完成[Host 暂存准备](../../start/installation.md#local-prerequisites)。

## 运行脚本 {#run-script}

<div class="pv-guide-steps" markdown="1">

1. ### 准备脚本 {#prepare-script}

    在新目录中创建脚本。若目录已存在，换个名字再继续；后续步骤在同一终端和目录执行。

    ```bash title="Shell"
    mkdir pvisor-script-demo
    cd pvisor-script-demo
    cat > agent.sh <<'SCRIPT'
    #!/bin/sh
    set -eu
    mkdir -p output
    echo 'Report generated' > output/report.txt
    echo 'Temporary notes' > scratch.txt
    SCRIPT
    chmod +x agent.sh
    ```

1. ### 运行并检查报告 {#inspect-script}

    ```bash title="Shell"
    pvisor spawn --safe --overlaynet-deny-all -- ./agent.sh
    pvisor status --review last
    pvisor inspect last -- cat output/report.txt
    ```

    最后一条命令应输出 `Report generated`。审查记录应列出 `output/report.txt` 和 `scratch.txt`；原目录此时尚无报告或笔记。命令失败也可能留下候选文件，先检查记录再决定。

1. ### 接收报告并清理 {#accept-report}

    ```bash title="Shell"
    pvisor apply last --path output
    pvisor drop last
    cat output/report.txt
    test ! -e scratch.txt && echo 'scratch.txt: absent'
    ```

    应输出报告内容和 `scratch.txt: absent`。报告已进入原目录，临时笔记已丢弃，原脚本保留。

</div>

## 行为

- 命令在暂存视图中运行，能看到项目当前的文件，写入进入暂存区。
- `pvisor spawn` 原样返回命令的退出码，脚本和 CI 可据此判断成败。
- 命令非零退出时，文件改动仍保留在暂存区，可以审查后决定 apply 或 drop。
- `--` 之后的内容原样作为命令和参数传递。

## 网络

脚本不匹配任何 Agent 预设时，`--safe` 默认拒绝所有出站。按需选择：

| 需要 | 参数 |
| --- | --- |
| 完全离线 | `--overlaynet-deny-all` |
| 只访问少数目标 | `--overlaynet-allow pypi.org:443`（可重复） |
| 拒绝特定目标 | `--overlaynet-deny 169.254.0.0/16` |

`bash`、`sh`、`zsh`、`fish` 作为命令名时沿用 Codex 的预设目标，见 [`--safe` 参数预设](../../reference/cli.md#safe-参数预设)。

## 环境变量

`--safe` 只投影必要变量。脚本需要的变量用 `--pass-env NAME` 显式传入，见[凭据与环境变量](../policies/credentials.md)。

## 完整示例

[第一次运行](../../start/first-run.md)提供离线文件暂存与选择性接收的演示；[`examples/pvisor/`](https://github.com/DeepLink-org/pvisor/tree/main/examples/pvisor) 下有文件隔离、改动集管理和网络隔离的可运行示例。
