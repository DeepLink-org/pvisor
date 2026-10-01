# SandboxReplay

SandboxReplay 是 pVisor 的 Agent 轨迹回放能力。它面向用户已经创建好的新沙箱，重新执行原始轨迹中恢复边界之前的工具调用，用新沙箱产生的 observation 重建 Agent 原生上下文，然后从边界后继续运行。它会在
[Run 与 Attempt 模型](../concepts/run-model.md)中创建派生 Run。

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

## 3. Agent 适配

### 3.1 Claude Code

Claude Code 使用原生 JSONL/UUID session。SandboxReplay 解析活动 parent UUID 链，重放前 N 个完整工具批次，将新 observation 写入重建 session，然后通过 `claude --resume` 启动续跑。

Claude Code 的 resume transport 会在模型请求前插入临时消息，因此 SandboxReplay 启动一个仅供本次续跑使用的本地协议桥。该桥验证并删除准确匹配的临时 envelope，使第一次模型请求仍结束在 `O′N`。它不启用 pVisor Gateway，也不捕获或持久化模型流量。

异步子 Agent 的 `Agent` 与 `TaskOutput` 被视为 Claude Code 原生工具。恢复边界必须选在能够由原生 session 无歧义重建的位置。

### 3.2 OpenHands

OpenHands 使用原生 event trajectory 和 ReplayManager。SandboxReplay 提取前 N 个 Action，OpenHands Runtime 在新沙箱中执行这些 Action 并产生新的 Observation；replay queue 耗尽后，OpenHands 直接发起续跑请求。

### 3.3 mini-swe-agent

mini-swe-agent 使用原生 `mini-swe-agent-1.1` messages。配套 runner 保留原始 system 和任务消息，在新沙箱中重放前 N 个 action，并使用原生 observation formatter 将结果加入 `agent.messages`，随后直接调用下一次 `agent.step()`。

### 3.4 Pi agent

Pi agent 适配固定支持 `@earendil-works/pi-coding-agent` `0.83.0`，输入为
Pi 原生 RPC event JSONL。一个 replay step 对应一个完整的 `turn_end` 工具批次。
SandboxReplay 使用 Pi 自身的工具实现重新执行 `read`、`bash`、`edit`、`write`，
将新 observation 写入新建的 Pi v3 session，再通过 Pi SDK 从边界续跑。轨迹包含
这四种工具之外的调用时会拒绝执行，避免静默改变工具语义。未配置边界提示词时调用
Pi 的原生 `continue()`；配置后则在 `O′N` 后通过 `prompt()` 追加一次用户消息。

### 3.5 OpenCode

OpenCode 适配固定支持 `1.17.7`，输入为 `opencode run --format=json` 产生的原生
事件 JSONL。`user`、`step_start`、`text`、`reasoning`、`tool_use` 和
`step_finish` 事件会按 step 分组；一个 replay step 对应一个完整的工具调用批次。
SandboxReplay 在新沙箱中重新执行命令型工具以及 `read`、`write`、`edit` 文件工具，
用新的 `state.output` 重建前缀，并通过 `opencode run --format=json --session` 从边界
继续。工具执行期间的中间 `tool_use` 状态会合并，避免同一调用被重复回放。

`opencode run` 没有 CLI 步数参数，且 1.17.7 在 resume 会话上不执行
`agent.build.steps` 配置预算。SandboxReplay 因此为 OpenCode 引入了两个外部看门狗
（对其它 Agent 不启用）。

续跑步数由 pVisor 监督 stderr 进度日志，达到剩余预算后发送 SIGINT。
连续 10 分钟无输出会触发空闲终止。预算触发结果为 `agent_status = max_steps`，
metadata 记录 `opencode_step_budget`。这些机制针对固定适配版本；
原始实测和诊断见 [实验报告](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/replay/qwen3.6-results.md)。

### 3.6 Codex

Codex 适配固定支持 CLI `0.149.0`，输入为 Codex 原生 rollout JSONL。一个 replay step
对应一组完整的 `function_call`/`custom_tool_call` 及其 outputs。SandboxReplay 在新沙箱
中重放前缀工具，将原生 `response_item` 前缀写入隔离的 `CODEX_HOME`，再执行
`codex exec resume <session-id> --json`。native session ID 从 `session_meta` 自动提取；
未知工具或缺少 native session ID 时显式失败，不会退化成全新会话。

Codex 保留自身的 system prompt、工具定义和任务上下文，SandboxReplay 只替换边界前的
observation。为兼容需要 resume prompt 的旧版 CLI，SandboxReplay 使用本地 Responses
bridge 删除 transport nonce，再将请求转发到模型服务；续跑结束后也会从 native trajectory
中清理 nonce。默认模式的输入条件为 `replayed_boundary_only`，不会向模型注入额外提示词。

配置 `boundary_user_prompt` 时，提示词只在 `O′N` 后注入一次并保留；此时输入条件标记为
`boundary_user_prompt_appended`。bridge 校验失败会拒绝续跑，不降级为直连。

## 4. 使用方式

### 4.1 安装 pVisor

pVisor CLI 随 `pvisor` wheel 发布。沙箱内有 Python 3.10 或更高版本时，推荐直接
安装发布版：

~~~bash
python -m pip install pvisor

command -v pvisor
pvisor --version
pvisor replay --help
~~~

如果需要测试尚未发布的 SandboxReplay 代码，可以在目标沙箱中从源码只安装 pVisor：

~~~bash
git clone https://github.com/DeepLink-org/pvisor.git
cd pvisor
just install-cli

export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
pvisor replay --help
~~~

开发时也可以不安装，直接构建并使用仓库内二进制：

~~~bash
just build release
./target/release/pvisor replay --help
~~~

`pvisor replay` 通常直接运行在用户已经创建的新沙箱中，因此 pVisor 必须安装在该
沙箱内，或者以只读方式挂载到沙箱的 `PATH`。如果在沙箱外构建后复制二进制，构建机
与目标沙箱的操作系统、CPU 架构和动态链接运行时必须兼容。Agent runtime 也必须与
所选 replay profile 的固定版本一致。

### 4.2 CLI 与 TOML

SandboxReplay 默认假设用户已经创建了一个新沙箱，并在沙箱中直接运行：

~~~bash
pvisor replay \
  --agent claude-code \
  --trajectory /input/session.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/claude \
  --boundary-user-prompt '请检查新的 observation 后继续任务'
~~~

Pi agent runtime 安装在 `/opt/pi-agent` 时，命令为：

~~~bash
pvisor replay \
  --agent pi-agent \
  --trajectory /input/pi-agent.events.jsonl \
  --after-step 30 \
  --agent-entrypoint /opt/pi-agent/bin/pi
~~~

Pi agent 等价的 pVisor TOML 配置为：

~~~toml
[replay]
agent = "pi-agent"
trajectory = "/input/pi-agent.events.jsonl"
after_step = 30
agent_entrypoint = "/opt/pi-agent/bin/pi"
max_steps = 200
disable_thinking = true
~~~

OpenCode 使用原生事件 JSONL：

~~~bash
pvisor replay \
  --agent opencode \
  --trajectory /input/opencode.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/opencode
~~~

Codex 使用原生 rollout JSONL；SandboxReplay 会自动读取轨迹中的 Codex native
session ID，用户无需手工填写 `session_id`：

~~~bash
pvisor replay \
  --agent codex \
  --trajectory /input/rollout.jsonl \
  --after-step 30 \
  --agent-entrypoint /usr/bin/codex
~~~

两者也可使用同一个 TOML 文件，只需将 `[replay].agent`、`trajectory` 和
`agent_entrypoint` 分别改为 `opencode` 或 `codex`。

Claude Code 等价的 pVisor TOML 配置为：

~~~toml
[replay]
agent = "claude-code"
trajectory = "/input/session.jsonl"
after_step = 30
agent_entrypoint = "/usr/bin/claude"
max_steps = 200
session_id = "task-291-attempt-1"
replay_only = false
disable_thinking = true
boundary_user_prompt = "请检查新的 observation 后继续任务"
~~~

### 4.3 执行模式与结果

- 默认模式会执行选中的前缀，然后启动 Agent 继续运行；
- `--replay-only` 会执行前缀，但在下一次模型请求前停止；
- `--prepare-only` 只校验并构造前缀，不执行工具、不启动 Agent，也不要求 Agent runtime。

`--max-steps` 是包含回放前缀在内的 Agent 动作总预算。例如
`--after-step 30 --max-steps 50` 最多留下 20 个续跑动作。仅回放模式的预算必须覆盖前缀；续跑模式还必须至少留下一个实时动作。

结果协议为 `sandbox-playback.result/v3`：`phase` 为 `prepared`、`replayed`
或 `continued`；`quality` 为 `verified` 或 `degraded`；`agent_status` 区分
`not_started`、`completed`、`max_steps` 与 `failed`。失败结果会保留已经生成的日志和原生轨迹。即使 OpenHands 进程返回 0，只要控制器报告 fatal 状态，结果仍为失败。

成功结果的 metadata 记录边界提示词是否请求和注入，以及字符长度和 SHA-256；
replay journal 不记录提示词明文；Agent 原生的 prepared 或 continued trajectory
可能包含这条 user 消息。对于 Claude Code，内存桥只把提示词加入第一次清理后的
上游请求，不修改重建的原生 session。配置提示词后，
`next-action-comparison.json` 的输入条件为 `boundary_user_prompt_appended`。
此时文本相似度和工具一致性仅供观察，不能解释为相同输入下的 replay 一致性。

无法重新生成的 Claude observation 默认失败，只有显式指定 `--allow-stale-observations` 才会复用并把质量标记为 `degraded`。

`disable_thinking` 也可以通过 `--disable-thinking` 指定。只有显式提供 `--executor`、`--stage` 或 `--mount` 等运行参数，或在 TOML 中增加 `[run]`、`[filesystem]`、`[overlaynet]`，才会在回放外层创建受管的 `pvisor run`。

完整参数见 [`pvisor replay` 命令参考](../reference/cli.md#replay-an-agent-trajectory)。

## 实验与开发资料

使用方法和保证以上面的回放语义及固定适配版本为准。
[Qwen3.6 实验报告](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/replay/qwen3.6-results.md)
保留逐题结果、下一动作比较和当时的 OpenCode 诊断，不作为兼容性或确定性保证。
