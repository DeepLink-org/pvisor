# 回放历史轨迹

SandboxReplay 在用户已创建的新沙箱中回放 Agent 轨迹：重新执行恢复边界之前的工具调用，用新沙箱产生的 observation 重建 Agent 原生上下文，再从边界后继续运行。它在 [Run 与 Attempt 模型](../design/execution-model.md) 中创建派生 Run。

## 1. 基本概念

一条 Agent 轨迹可以表示为：

~~~text
任务 → A1 → O1 → ... → AN → ON → A(N+1) → ...
~~~

- `Ai`：模型产生的第 i 个动作，可包含可见文本、reasoning 和一个或多个工具调用；
- `Oi`：执行 `Ai` 后返回给模型的 observation；
- `N`：选定的恢复边界；
- `A(N+1)`：原始轨迹在恢复边界后的下一次模型回复；
- `A′(N+1)`：在新沙箱回放前 N 个动作后，模型生成的第一次续跑回复。

如果一个 assistant response 包含多个并行工具调用，它们属于同一个动作批次，恢复边界不能位于批次内部。

## 2. 回放语义

SandboxReplay 依次完成两件事：

1. 在新沙箱中按原顺序重新执行 `A1...AN` 的工具调用，产生当前沙箱真实的 `O′1...O′N`；
2. 保留 Agent 原生 system prompt、工具定义、任务和历史动作，用 `O′1...O′N` 替换旧 observation，然后直接请求下一次模型推理。

第一次续跑请求必须准确结束在 `O′N`：

~~~text
Agent 原生 system prompt + 工具定义 + 原始任务
    + A1 → O′1 → ... → AN → O′N
                              ^ 请求在这里结束
~~~

`O′N` 后不得添加“请继续”“Continue from where you left off”等额外消息。回放保证恢复流程和消息边界正确，但不保证 `A′(N+1)` 与 `A(N+1)` 逐字一致；文件状态、工具输出中的动态字段以及模型采样都可能改变下一动作。

如果用户明确希望改变边界后的第一次推理，可以配置
`boundary_user_prompt`：

~~~text
Agent 原生 system prompt + 工具定义 + 原始任务
    + A1 → O′1 → ... → AN → O′N
    + boundary_user_prompt → A′(N+1)
~~~

该提示词只在 `O′N` 之后、第一次实时模型推理之前注入一次，不替换原始任务。
`prepare-only` 和 `replay-only` 不发起实时模型请求，因此不会注入。未配置时仍保持
“请求准确结束在 `O′N`”的原有语义。

## 3. 安装与运行位置

`pvisor replay` 通常直接运行在用户已经创建的新沙箱中，因此 pVisor 必须安装在该沙箱内，或者以只读方式挂载到沙箱的 `PATH`。沙箱内有 Python 3.10 或更高版本时，直接安装发布版：

~~~bash
python -m pip install pvisor

command -v pvisor
pvisor --version
pvisor replay --help
~~~

如果在沙箱外构建后复制二进制，构建机与目标沙箱的操作系统、CPU 架构和动态链接运行时必须兼容。Agent runtime 也必须与所选 replay profile 的固定版本一致；各适配器的机制与固定版本见[回放设计](../design/replay.md)。

## 4. 最小用法

~~~bash
pvisor replay \
  --agent claude-code \
  --trajectory /input/session.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/claude
~~~

执行模式（默认续跑、`--replay-only`、`--prepare-only`）、步数预算、边界提示词、结果协议 `sandbox-playback.result/v3` 与各 Agent 的 CLI/TOML 写法，以 [`pvisor replay` 命令参考](../reference/cli.md#replay-an-agent-trajectory)为准。

## 实验与开发资料

使用方法和保证以上面的回放语义及固定适配版本为准。
[Qwen3.6 实验报告](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/replay/qwen3.6-results.md)
保留逐题结果、下一动作比较和当时的 OpenCode 诊断，不作为兼容性或确定性保证。
