---
description: "在 pVisor 下运行 Claude Code：--safe 放行哪些网络目标、如何传入 ANTHROPIC_API_KEY，以及如何审查并应用它的文件改动。"
---

# Claude Code

用 Claude Code 修复一个空白字符处理有误的 Python 函数。测试通过后，只接收源码和测试，丢弃实验笔记。任务使用 Host stagebox、Python 标准库和你自己的模型账号。

## 完整任务：修复失败测试 {#repair-task}

<div class="pv-guide-steps" markdown="1">

1. ### 准备工具和认证 {#task-prerequisites}

    先完成[Host 暂存准备](../../start/installation.md#local-prerequisites)和[离线演示](../../start/first-run.md#agent-style)。在当前终端确认版本：

    ```bash title="Shell"
    pvisor --version
    claude --version
    python3 --version
    ```

    记录版本以便复现。Claude Code 必须已安装在本机，Python 为 3.10+。通过你已有的凭据管理方式在当前终端设置 `ANTHROPIC_API_KEY`；启动时用 `--pass-env` 显式传入。不要把 Key 写入项目文件。模型调用使用你自己的账号额度；登录会话与其他 provider 配置见下方[凭据](#credentials)。

1. ### 创建有失败测试的小项目 {#task-project}

    使用一个新目录；若目录已存在，换名再运行。下面的所有步骤都在该目录和同一个终端中执行，无需 Git 或第三方依赖。

    ```bash title="Shell"
    mkdir pvisor-agent-demo
    cd pvisor-agent-demo
    cat > slugify.py <<'PYCODE'
    def slugify(text):
        return text.lower().replace(" ", "-")
    PYCODE
    cat > test_slugify.py <<'PYCODE'
    import unittest
    from slugify import slugify


    class SlugifyTests(unittest.TestCase):
        def test_words(self):
            self.assertEqual(slugify("Hello World"), "hello-world")

        def test_trim(self):
            self.assertEqual(slugify("  Hello World  "), "hello-world")

        def test_whitespace(self):
            self.assertEqual(slugify("Hello\t \nWorld"), "hello-world")
    PYCODE
    python3 -B -m unittest -v
    ```

    预期运行 3 个测试、2 个失败，退出码非零，接下来让 Agent 修复这些失败。若出现导入或语法错误，先检查文件内容再启动 Agent。

1. ### 启动 Agent 并交付任务 {#task-agent}

    ```bash title="Shell"
    pvisor spawn --safe --pass-env ANTHROPIC_API_KEY -- claude
    ```

    在 Agent 中粘贴以下任务：

    ```text
    修复 slugify.py 中的 slugify：转为小写，删除首尾空白，把连续空白（包括 tab、换行）合并为一个连字符。
    保留 test_slugify.py 的三个现有测试，并为纯空白字符串返回空字符串新增一个测试。
    只修改 slugify.py、test_slugify.py；另外创建 experiment-notes.txt 简述尝试过程，作为不接收的实验记录。
    不安装依赖，不访问外部服务，不执行 Git 提交或推送。
    先运行测试确认失败，修复后运行 python3 -B -m unittest -v，并报告实际结果。
    ```

    Agent 可能要求工具审批；根据实际提示决定是否允许，不启用未经验证的全放行参数。记录提示和人工介入次数。模型认证或连接失败时，保留错误并核对下方网络与凭据配置。任务完成后退出 Agent，回到原终端。

1. ### 检查成果和实验记录 {#task-review}

    ```bash title="Shell"
    pvisor status last
    pvisor review last --diff
    pvisor inspect last -- python3 -B -m unittest -v
    ```

    预期为 4 个测试通过。这里 `-B` 禁止生成字节码缓存，测试仅使用标准库并读取文件，可以在只读 inspect 视图运行；不能把这一方式套用于需要写缓存或产物的任意测试。

    对照任务核对差异：原有三个测试应保持完整，新增纯空白测试，实现符合需求。检查 `experiment-notes.txt` 是否只包含实验说明。Agent 的回答不能代替文件审查。此时原目录中的源码和测试仍应是原始版本。

1. ### 选择性接收并在原目录复测 {#task-accept}

    ```bash title="Shell"
    pvisor apply last --path slugify.py --path test_slugify.py
    python3 -B -m unittest -v
    test ! -e experiment-notes.txt && echo 'Experiment notes not accepted'
    ```

    应再次看到 4 个测试通过，并输出 `Experiment notes not accepted`。接收的文件应能在原项目中运行，不依赖 Agent 的实验环境。若验证失败，保留 stage 并检查是否遗漏依赖或候选修改。

    如果审查确认 `experiment-notes.txt` 仍是未接受的候选，清理它：

    ```bash title="Shell"
    pvisor drop last
    python3 -B -m unittest -v
    ```

    应仍为 4 个测试通过。若 Agent 没有创建笔记、全部候选已接受，则跳过 drop；若出现其他文件，先审查再决定。命令失败或取消后也先检查 Job 状态，保留可用记录，再按[故障排查](../troubleshooting.md)处理。

</div>

## 验证与审批范围 {#verification}

| 项目 | 已有证据或验收要求 |
| --- | --- |
| 真实客户端工具循环 | 已有记录使用 Claude Code 2.1.128 和本地确定性模型响应；配置与范围见[端到端任务](../../benchmarks/agent-tasks.md)。 |
| 本教程的云模型修复任务 | 仍需使用发布制品完成云模型端到端验收。 |
| 无反复审批体验 | 尚未建立已验证版本与配置矩阵；不能由工具循环测试推断。 |
| 复现记录 | 保留系统、pVisor/Claude 版本、启动命令、测试前后结果、Job 身份和实际审批提示；不包含 Key。 |

当前教程保留 Claude 自身审批设置。首轮认证、工具权限或其他确认仍可能出现；文件 review/apply 不撤销模型费用或远端操作。源码和测试是要交付的成果，笔记是本例人为指定的实验记录，产品当前不会自动分类或认证文件安全。

## `--safe` 为 Claude Code 做了什么

网络预设按命令名 `claude` 匹配，只放行 `api.anthropic.com:443`，其他目标默认拒绝。工作区暂存、HOME 状态丢弃和敏感路径规则沿用[Agent 通用设置](index.md)。

## 凭据 {#credentials}

`--safe` 不会把宿主环境中的凭据传给 Agent。二选一：

- 用 `--pass-env ANTHROPIC_API_KEY` 显式交付；
- 配置 Gateway 路由，由可信侧持有上游 Key，Agent 看不到 Key，见[凭据与环境变量](../policies/credentials.md)。

macOS 上 `--safe` 使用临时 HOME，宿主上已登录的会话状态不可见；Linux 上 HOME 通过私有 stage 投影，Agent 写入的状态不会回到宿主。

## 需要访问其他目标时

Agent 需要安装依赖或访问文档站点时，显式追加目标。注意 `--overlaynet-allow` 会覆盖预设列表，所以要把模型 API 一起写上：

```bash title="Shell"
pvisor spawn --safe \
  --overlaynet-allow api.anthropic.com:443 \
  --overlaynet-allow pypi.org:443 \
  --pass-env ANTHROPIC_API_KEY -- claude
```

## 网络边界的强度

在 macOS host 上，`--safe` 阻断直接外部连接；在 Linux host 上，选择性规则通过协作式代理执行，直接 socket 仍可能绕过。需要不可绕过的边界时使用 VM，见[网络边界](../policies/network.md#网络边界)。
