# 回放设计

Replay 把“Agent 曾经调用过哪些工具”转换成“这些工具在当前工作区实际产生了什么结果”，再把新结果交给 Agent 继续执行。它适合复现一次失败、比较修复后的行为，以及构造训练前缀。

使用步骤见[轨迹回放指南](../guides/replay.md)。这里说明边界选择、工具重执行和各 Agent 原生会话之间的关系，方便你判断一次回放结果是否可比。

## 当前处理流水线

`engine.rs::execute` 校验请求与目录，调用 `adapter::build_plan` 读取原生轨迹并选择完整工具批次，校验步数预算与固定 Agent 版本，再为本次回放分配独立 state/output 目录。`after_step` 必须在已有完整批次范围内，不能把同一轮的工具调用切开。

| 阶段 | 作用与产物 |
| --- | --- |
| Prepare | 解析历史、确定边界并生成重建文件；`--prepare-only` 不执行历史工具，也不启动实时 Agent |
| Replay | 在当前 workspace 重新执行支持的工具前缀，用 fresh observation 替换旧结果；`--replay-only` 在边界停止 |
| Continue | 把重建的原生上下文交给指定版本的 Agent，校验首个续跑请求使用了该前缀；边界提示在 fresh observation 之后注入 |
| Finalize | 保存结果、质量、失败、重建轨迹与续跑产物；保留可定位的失败信息 |

`adapter/` 负责轨迹解析、工具语义和启动选择；`bridge/` 负责 Claude、Codex、OpenCode 的模型协议桥。各适配器的机制与固定版本见下文；固定版本是"已验证该版本"的声明，不是"兼容任意新版"的承诺。

## 适配器机制与固定版本

### Claude Code

Claude Code 使用原生 JSONL/UUID session。SandboxReplay 解析活动 parent UUID 链，重放前 N 个完整工具批次，将新 observation 写入重建 session，然后通过 `claude --resume` 启动续跑。

Claude Code 的 resume transport 会在模型请求前插入临时消息，因此 SandboxReplay 启动一个仅供本次续跑使用的本地协议桥。该桥验证并删除准确匹配的临时 envelope，使第一次模型请求仍结束在 `O′N`。它不启用 pVisor Gateway，也不捕获或持久化模型流量。

异步子 Agent 的 `Agent` 与 `TaskOutput` 被视为 Claude Code 原生工具。恢复边界必须选在能够由原生 session 无歧义重建的位置。

### OpenHands

OpenHands 使用原生 event trajectory 和 ReplayManager。SandboxReplay 提取前 N 个 Action，OpenHands Runtime 在新沙箱中执行这些 Action 并产生新的 Observation；replay queue 耗尽后，OpenHands 直接发起续跑请求。

### mini-swe-agent

mini-swe-agent 使用原生 `mini-swe-agent-1.1` messages。配套 runner 保留原始 system 和任务消息，在新沙箱中重放前 N 个 action，并使用原生 observation formatter 将结果加入 `agent.messages`，随后直接调用下一次 `agent.step()`。

### Pi agent

Pi agent 适配固定支持 `@earendil-works/pi-coding-agent` `0.83.0`，输入为
Pi 原生 RPC event JSONL。一个 replay step 对应一个完整的 `turn_end` 工具批次。
SandboxReplay 使用 Pi 自身的工具实现重新执行 `read`、`bash`、`edit`、`write`，
将新 observation 写入新建的 Pi v3 session，再通过 Pi SDK 从边界续跑。轨迹包含
这四种工具之外的调用时会拒绝执行，避免静默改变工具语义。未配置边界提示词时调用
Pi 的原生 `continue()`；配置后则在 `O′N` 后通过 `prompt()` 追加一次用户消息。

### OpenCode

OpenCode 适配固定支持 `1.17.7`，输入为 `opencode run --format=json` 产生的原生
事件 JSONL。`user`、`step_start`、`text`、`reasoning`、`tool_use` 和
`step_finish` 事件会按 step 分组；一个 replay step 对应一个完整的工具调用批次。
SandboxReplay 在新沙箱中重新执行命令型工具以及 `read`、`write`、`edit` 文件工具，
用新的 `state.output` 重建前缀，并通过 `opencode run --format=json --session` 从边界
继续。工具执行期间的中间 `tool_use` 状态会合并，避免同一调用被重复回放。

`opencode run` 没有 CLI 步数参数，且 1.17.7 在 resume 会话上不执行
`agent.build.steps` 配置预算。SandboxReplay 因此为 OpenCode 引入了两个外部看门狗
（对其它 Agent 不启用）：续跑步数由 pVisor 监督 stderr 进度日志，达到剩余预算后发送
SIGINT；连续 10 分钟无输出会触发空闲终止。预算触发结果为 `agent_status = max_steps`，
metadata 记录 `opencode_step_budget`。这些机制针对固定适配版本；
原始实测和诊断见 [实验报告](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/replay/qwen3.6-results.md)。

### Codex

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

回放历史工具会再次产生文件、命令和可能的远程副作用。文件检查点不保存这些外部状态；旧 observation 无法刷新时，默认拒绝，只有显式允许才记录降级。新上下文中的工具输出可以不同，模型继续时也可以选择不同动作，不能把成功续跑理解为确定性再现原轨迹。

Claude 历史 `Read` 只接受 workspace 内的普通文件。它保留指向 workspace 内部的既有 symlink 解析，但实际打开时以目录 FD 逐段拒绝新的 symlink，并以非阻塞打开后的 inode 类型检查拒绝 FIFO、目录和设备；FIFO 不需要 writer 或外层 Run timeout 才能返回工具错误。

每次 `Read` 最多扫描 16 MiB（另探测一个字节以确认截断），最多返回 2,000 行文件正文，整个工具正文不超过 4 MiB。`offset` 从 1 开始，显式 `limit` 在这些预算内保持原义；达到扫描、行数或输出字节预算时保留已读内容，追加一行 `Read truncated by pvisor replay` 提示，并设置 observation 的 `truncated=true`。因此含提示的结果最多 2,001 行。

若显式请求的范围已在扫描边界前完整取得，不因文件其他部分很大而标记截断。高 offset 超出扫描范围也会显示截断，不能将该结果解释为完整文件的 EOF。

普通文件仍可能被并发修改；这些预算不构成文件快照，也不保证远程或故障文件系统的 I/O 延迟。

## 代码与验证

```bash
just test pvisor-replay
```

- `adapter/claude_code.rs`：完整批次分组、prepare-only 无工具执行、旧 observation 拒绝或显式降级、Read 范围与输出预算、历史命令超时与进程组清理；
- `io.rs`：Read 普通文件检查、目录 FD 路径约束及解析后的 symlink 替换回归；
- `adapter/generic.rs`：Codex/OpenCode 的原生身份、完整工具轮次、续跑前缀验证与传输 nonce 过滤；
- `tests/replay_contract.rs`：mini-swe-agent、SWE-agent、OpenHands、Pi 的 replay-only 边界、提示注入与步数约束；
- `bridge/`：协议桥的请求/响应映射及边界验证。

六种适配器的原生前缀准备实验与逐项结果见[回放保真度](../benchmarks/replay-fidelity.md)。该实验验证前缀和零执行准备；模型续跑质量需要按真实任务另行测量。
