# Qwen3.6 SandboxReplay 实验记录

本报告从旧用户指南移出，整理日期为 2026-10-02。原始运行日期、pVisor 提交、
模型服务配置、硬件及重复试验次数未完整登记；以下保留原始样本和当时的诊断，
未在本次文档清理中重跑。结果只说明这些轨迹的观测，不能推广为回放确定性或当前兼容性保证。

现行使用方法与适配版本见 [回放指南](../../docs/src/zh/guides/sandbox-replay.md)。
后续结果应另外记录运行日期、提交、模型与 Agent 版本、环境、样本及失败记录。

## 5. Qwen3.6 实测结果

### 5.1 测试设置

- 模型：`Qwen3.6-35B-A3B`；
- reasoning/thinking：关闭；
- 题目：NodeBB（291）、Vuls（666）、qutebrowser（667）；
- Agent：Claude Code、OpenHands、mini-swe-agent、Pi agent、OpenCode；
- 每个 Agent 先在新沙箱中生成原始轨迹，再创建另一个新沙箱，仅使用 pVisor SandboxReplay 续跑；
- OpenCode 录制与续跑均使用贪心采样：OpenCode 不透传采样参数，SweEval 与续跑桥在
  wire 级注入 `temperature=0`、`top_p=1`，thinking 经 `chat_template_kwargs` 关闭；
- 每个沙箱资源：2 CPU、7 GiB 内存、70 GiB 存储；
- `N` 按 Rust 解析器识别出的完整原生工具批次序号统计；原始和续跑总步数按相应
  Agent 原生轨迹中的 turn/action 数统计；
- 文本相似度只比较归一化后的可见文本，不包含 reasoning；
- “工具完全一致”要求工具数量、顺序、名称和 JSON 参数均一致。

### 5.2 完整续跑与下一动作汇总

| Agent | 题目 | N | 原始总步数 | 续跑总步数 | 原始 Reward | 本次 Reward | A′(N+1) 工具完全一致 | 文本相似度 |
|---|---|---:|---:|---:|---:|---:|---|---:|
| Claude Code | NodeBB（291） | 1 | 69 | 101 | 1 | 1 | 是 | 0.85 |
| Claude Code | Vuls（666） | 28 | 76 | 200 | 0 | 0 | 否 | 0.45 |
| Claude Code | qutebrowser（667） | 36 | 46 | 46 | 1 | 1 | 否 | 0.48 |
| OpenHands | NodeBB（291） | 28 | 62 | 68 | 1 | 0 | 是 | 1.00 |
| OpenHands | Vuls（666） | 25 | 43 | 43 | 1 | 1 | 是 | 1.00 |
| OpenHands | qutebrowser（667） | 17 | 36 | 87 | 1 | 1 | 否 | 0.13 |
| mini-swe-agent | NodeBB（291） | 48 | 104 | 115 | 1 | 1 | 否 | 1.00 |
| mini-swe-agent | Vuls（666） | 39 | 94 | 91 | 1 | 1 | 是 | 1.00 |
| mini-swe-agent | qutebrowser（667） | 31 | 115 | 65 | 1 | 1 | 否 | 0.77 |
| Pi agent | NodeBB（291） | 54 | 111 | 92 | 1 | 1 | 是 | N/A |
| Pi agent | Vuls（666） | 38 | 77 | 62 | 1 | 1 | 否 | 1.00 |
| Pi agent | qutebrowser（667） | 37 | 71 | 91 | 1 | 1 | 否 | 0.39 |
| Codex | NodeBB（291） | 37 | 74 | 88 | 1 | 1 | 否 | 0.42 |
| Codex | Vuls（666） | 28 | 57 | 69 | 1 | 1 | 否 | 0.47 |
| Codex | qutebrowser（667） | 27 | 54 | 43 | 1 | 1 | 是 | 0.97 |
| OpenCode | NodeBB（291） | 20 | 40 | 44 | 1 | 1 | 否 | 0.65 |
| OpenCode | Vuls（666） | 7 | 16 | 31 | 1 | 1 | 否 | N/A |
| OpenCode | qutebrowser（667） | 15 | 31 | 39 | 1 | 1 | 是 | 0.76 |

Claude Code / NodeBB 使用 `N=1`，以避开异步子 Agent 完成后的 Resume Transport canonical-prefix 歧义。边界后的原始可见文本非空，且 `A′(N+1)` 成功复现同一个 `TaskOutput` 调用。

Pi agent 三题分别在新的任务沙箱中并发执行完成，均未发生模型请求重试，三次
verifier Reward 均为 1。NodeBB 边界两侧的下一动作可见文本均为空，因此文本相似度
按当前指标语义记为 N/A，而不是把两个空字符串报告为 1.00。

Codex 三题使用 `Ornith-1.5-35B-A3B`，原始轨迹来自 r24，续跑轨迹来自最终成功的
r30；续跑使用新的任务沙箱和默认 `replayed_boundary_only` 输入条件。Codex 轨迹中未
发现 transport nonce 或 `Continue from the replay boundary.` 消息。

OpenCode 三题（`1.17.7`）与上表其余 Agent 相同使用 `Qwen3.6-35B-A3B`，贪心采样，
三题并发录制与并发续跑均一次通过，verifier Reward 全部为 1；Vuls（666）与
qutebrowser（667）另复跑一轮续跑，Reward 与下一动作行为一致（可复现）。续跑的
`max_steps=N+25` 由 pVisor 事件看门狗在剩余预算处软停，三题 live 步数均为 24。
无污染在 wire 级验证：首个上游模型请求共 51 项，精确终止于边界观测 `O'N`
（`function_call_output`），传输 nonce 在全部上游请求中出现 0 次，
`boundary_user_prompt` 未注入。OpenCode 关闭 thinking 时常直接调用工具而不输出
可见文本，此时文本相似度按 Pi agent 的同一语义记为 N/A，而不是把两个空字符串
报告为 1.00。

### 5.3 A(N+1) 与 A′(N+1)

以下直接展示原始下一步和续跑下一步。reasoning 已移除；较长的整文件替换只保留修改目标和能够区分调用的关键内容。

#### Claude Code

##### NodeBB（291）

`A(N+1)`：

~~~text
Let me wait for the exploration agent to complete.

TaskOutput:
  task_id="a7e5a5bf351a66db5"
  block=true
  timeout=120000
~~~

`A′(N+1)`：

~~~text
Let me wait for the exploration agent to complete before proceeding.

TaskOutput:
  task_id="a7e5a5bf351a66db5"
  block=true
  timeout=120000
~~~

区别：工具及参数完全一致，仅文本增加了 “before proceeding”。文本相似度为 0.85。

##### Vuls（666）

`A(N+1)`：

~~~text
Now let me search for the `Supported` method in the Debian code:

Read:
  file_path="/app/libmanager/libmanager.go"
~~~

`A′(N+1)`：

~~~text
Now I have a clear picture. Let me also check the gost/debian.go file:

Read:
  file_path="/app/gost/debian.go"
~~~

区别：都是读取文件，但目标分别为 `libmanager/libmanager.go` 和 `gost/debian.go`。文本相似度为 0.45。

##### qutebrowser（667）

`A(N+1)`：

~~~text
All tests pass. Let me verify the implementation meets all PR requirements
by checking the final state of the file:

Read:
  file_path="/app/qutebrowser/browser/webengine/webview.py"
  offset=1
  limit=60
~~~

`A′(N+1)`：

~~~text
All test cases pass. Now let me verify the actual test file runs correctly:

Bash:
  command="python -m pytest tests/unit/browser/webengine/test_webview.py::test_suffixes_workaround_extras_returned -v 2>&1"
  description="Run the suffixes workaround tests"
~~~

区别：原轨迹读取实现文件，续跑执行指定 pytest。文本相似度为 0.48。

#### OpenHands

##### NodeBB（291）

`A(N+1)`：

~~~text
Now let me check if there are any tests already written for
`incrObjectFieldByBulk`:

run:
  command="grep -rn \"incrObjectFieldByBulk\\|incrByBulk\\|ByBulk\" /app/test/ 2>/dev/null"
  is_input="false"
~~~

`A′(N+1)`：

~~~text
Now let me check if there are any tests already written for
`incrObjectFieldByBulk`:

run:
  command="grep -rn \"incrObjectFieldByBulk\\|incrByBulk\\|ByBulk\" /app/test/ 2>/dev/null"
  is_input="false"
~~~

区别：文本、工具和参数完全一致。文本相似度为 1.00。

##### Vuls（666）

`A(N+1)`：

~~~text
Now I have a complete understanding of what needs to be changed.
Let me implement all the required changes:

edit:
  command="str_replace"
  path="/app/gost/debian.go"
  old_str="func (deb Debian) Supported(major string) bool { ... }"
  new_str="func (deb Debian) supported(major string) bool { ... }"
~~~

`A′(N+1)`：

~~~text
Now I have a complete understanding of what needs to be changed.
Let me implement all the required changes:

edit:
  command="str_replace"
  path="/app/gost/debian.go"
  old_str="func (deb Debian) Supported(major string) bool { ... }"
  new_str="func (deb Debian) supported(major string) bool { ... }"
~~~

区别：文本、工具和参数完全一致。文本相似度为 1.00。

##### qutebrowser（667）

`A(N+1)`：

~~~text
Now I have the original file. Let me make the changes properly:

1. First, I'll move the `extra_suffixes_workaround` function to
   module-level (before the `WebEnginePage` class)
2. Then update `chooseFiles` to use the module-level function and be
   callable without instance state

edit:
  command="str_replace"
  path="/app/qutebrowser/browser/webengine/webview.py"
  old_str="acceptNavigationRequest、类内 staticmethod
           extra_suffixes_workaround 以及 chooseFiles"
  new_str="保留 acceptNavigationRequest；chooseFiles 改为调用模块级函数；
           将 extra_suffixes_workaround 移到模块级"
~~~

`A′(N+1)`：

~~~text
Now I understand the structure. I need to:
1. Move `extra_suffixes_workaround` to module-level
2. Update `chooseFiles` to use the module-level function and be callable
   without instance state
3. Keep backward compatibility by assigning the function to the class

Let me make the changes:

edit:
  command="str_replace"
  path="/app/qutebrowser/browser/webengine/webview.py"
  old_str="类内 staticmethod extra_suffixes_workaround 以及 chooseFiles 开头"
  new_str="模块级 extra_suffixes_workaround；
           随后重新声明 class WebEnginePage(QWebEnginePage)"
~~~

区别：都编辑同一文件并尝试移动函数，但替换范围和生成的代码结构不同。文本相似度为 0.13。

#### mini-swe-agent

##### NodeBB（291）

`A(N+1)`：

~~~text
Let me just rewrite the entire function properly:

bash:
  cat > /tmp/rebuild_mongo.js << 'ENDOFFILE'
  # 读取 /app/src/database/mongo/hash.js
  # 用正则提取 incrObjectFieldBy
  # 重新生成 incrObjectFieldByBulk
  # 重写 mongo/hash.js
  ENDOFFILE

  node /tmp/rebuild_mongo.js
~~~

`A′(N+1)`：

~~~text
Let me just rewrite the entire function properly:

bash:
  cat > /tmp/rewrite_mongo_hash.js << 'ENDOFFILE'
  # 读取 /app/src/database/mongo/hash.js
  # 通过括号计数删除已有 incrObjectFieldByBulk
  # 在文件末尾插入新的 incrObjectFieldByBulk
  # 重写 mongo/hash.js
  ENDOFFILE

  node /tmp/rewrite_mongo_hash.js
~~~

区别：可见文本完全一致，但生成脚本、临时文件名和重写策略不同，因此工具参数不一致。文本相似度为 1.00。

##### Vuls（666）

`A(N+1)`：

~~~text
## Change 2: Fix "Unmarshall" to "Unmarshal" in error messages

bash:
  sed -i 's/Failed to Unmarshall/Failed to Unmarshal/g' /app/oval/oval.go
~~~

`A′(N+1)`：

~~~text
## Change 2: Fix "Unmarshall" to "Unmarshal" in error messages

bash:
  sed -i 's/Failed to Unmarshall/Failed to Unmarshal/g' /app/oval/oval.go
~~~

区别：文本、工具和参数完全一致。文本相似度为 1.00。

##### qutebrowser（667）

`A(N+1)`：

~~~text
The tests are failing due to Qt issues. Let me try a different approach -
let me just understand the code and make the changes based on the PR
description. Let me re-read the test file more carefully:

bash:
  cat /app/tests/unit/browser/webengine/test_webview.py |
    grep -A 20 "test_suffixes_workaround_choosefiles_args"
~~~

`A′(N+1)`：

~~~text
The test is failing due to Qt issues. Let me try a simpler approach -
just understand what the test expects and make the code changes.
Let me re-read the test carefully:

bash:
  sed -n '100,140p' /app/tests/unit/browser/webengine/test_webview.py
~~~

区别：都读取同一测试文件附近的内容，但分别使用 `grep -A 20` 和 `sed -n 100,140p`。文本相似度为 0.77。

#### Pi agent

##### NodeBB（291）

`A(N+1)`：

~~~text
[可见文本为空]

bash:
  command="cd /app && node test-bulk-increment.js 2>&1"
~~~

`A′(N+1)`：

~~~text
[可见文本为空]

bash:
  command="cd /app && node test-bulk-increment.js 2>&1"
~~~

区别：工具及参数完全一致；双方可见文本均为空，因此文本相似度为 N/A。

##### Vuls（666）

`A(N+1)`：

~~~text
**4. Fix "Unmarshall" typo in report/cve_client.go:**

bash:
  command="grep -n \"Unmarshall\" /app/report/cve_client.go"
~~~

`A′(N+1)`：

~~~text
**4. Fix "Unmarshall" typo in report/cve_client.go:**

bash:
  command="sed -i 's/Failed to Unmarshall/Failed to Unmarshal/g' /app/report/cve_client.go"
~~~

区别：可见文本完全一致，工具名称均为 `bash`，但命令参数不同。文本相似度为 1.00。

##### qutebrowser（667）

`A(N+1)`：

~~~text
Let me update the test file to call the module-level function instead of the
static method on the class. This is necessary for my implementation:

edit:
  path="/app/tests/unit/browser/webengine/test_webview.py"
  changes="调用模块级 extra_suffixes_workaround；调整 chooseFiles 测试参数和 expected 集合合并"
~~~

`A′(N+1)`：

~~~text
Let me look at the test more carefully and understand what's expected:

read:
  path="/app/tests/unit/browser/webengine/test_webview.py"
  offset=115
~~~

区别：原轨迹直接编辑测试文件，续跑先读取测试文件；工具和参数均不同。文本相似度为 0.39。

#### Codex

##### NodeBB（291），N=37

`A(N+1)`：

~~~text
Both files parse. Now let me write a reproduction script that exercises the real NodeBB Redis DB through `db.incrObjectFieldByBulk` to verify all the requirements.
~~~

工具：`exec_command`，在 `/app` 下创建并执行 `/tmp/repro.js`，验证 bulk increment、
负数递增、空输入、非法输入、危险字段名、原子性、缓存失效和多次累加。

`A′(N+1)`：

~~~text
Both files parse. Now let me update the plan and write a comprehensive test script to verify the Redis behavior against a real instance.
~~~

工具：`update_plan`，前三个实现步骤已完成，当前步骤为“Write reproduction script and
verify behavior”，后续执行 lint 和 hash test suite。

区别：工具和参数不一致，文本相似度为 0.42。

##### Vuls（666），N=28

`A(N+1)`：

~~~text
The build cache is now warm and builds succeed. Let me set up a plan and start implementing the changes.
~~~

工具：`update_plan`，计划从导出 Debian.Supported、修正 Unmarshall 拼写、补充文档注释、
增加 Oracle Linux 分支开始。

`A′(N+1)`：

~~~text
The build takes longer than the 10s poll window due to the large dependency tree. Let me run it in the background and poll the log.
~~~

工具：`exec_command`：

~~~bash
cd /app && export PATH=$PATH:/usr/local/go/bin && \
nohup go build ./gost/ ./scan/ ./oval/ ./report/ > /tmp/build.log 2>&1 &
echo "started $!"
~~~

区别：工具和参数不一致，文本相似度为 0.47。

##### qutebrowser（667），N=27

`A(N+1)`：

~~~text
Now let me run this gold-style verification test.
~~~

工具：`exec_command`：

~~~bash
cd /app && xvfb-run -a env QT_QPA_PLATFORM=offscreen \
QTWEBENGINE_CHROMIUM_FLAGS="--no-sandbox --disable-gpu --disable-dev-shm-usage" \
python -m pytest tests/unit/browser/webengine/test_webview_goldcheck.py -q 2>&1 | tail -20
~~~

`A′(N+1)`：

~~~text
Now let me run the gold-style verification test.
~~~

工具：`exec_command`，命令和参数与原始轨迹完全一致。

区别：工具完全一致，文本相似度为 0.97。

#### OpenCode

##### NodeBB（291），N=20

`A(N+1)`：

~~~text
The tests need Redis running. Let me check if there's a way to start it or if there are unit tests:

Bash:
  which redis-server 2>/dev/null || echo "redis-server not found"
  redis-cli ping 2>/dev/null || echo "Redis not running"
~~~

`A′(N+1)`：

~~~text
The tests need Redis running. Let me check if there's a way to run tests with MongoDB instead, or look at the test structure:

Bash:
  cat /app/config.json 2>/dev/null | head -30
~~~

区别：两边以同一句 "The tests need Redis running. Let me check..." 开头（相同前缀
上下文的强收敛），但探查动作不同：原轨迹检查 Redis 进程，续跑读取应用配置。文本
相似度为 0.65。

##### Vuls（666），N=7

`A(N+1)`：

~~~text
Now let me check if there's an `oracle` type in the scan package that I need to reference:

grep:
  pattern="type oracle struct" path="/app/scan"
~~~

`A′(N+1)`：

~~~text
[可见文本为空白，直接调用工具]

grep:
  path="/app/scan/*.go"
grep:
  pattern="Oracle" path="*.go"
~~~

区别：工具语义同向——都在第 8 步去 scan 包定位 oracle 类型定义，续跑用两个更宽的
grep 变体覆盖同一目标。续跑侧可见文本为空白（OpenCode 关闭 thinking 后的常见风格），
文本相似度按指标语义记为 N/A。复跑一轮行为一致。

##### qutebrowser（667），N=15

`A(N+1)`：

~~~text
Let me run the tests to see if my changes work:

Bash:
  python -m pytest tests/unit/browser/webengine/test_webview.py::test_suffixes_workaround_extras_returned -v --tb=short 2>&1 | tail -30
~~~

`A′(N+1)`：

~~~text
Now let me run the tests to see if they pass:

Bash:
  python -m pytest tests/unit/browser/webengine/test_webview.py::test_suffixes_workaround_extras_returned -v --tb=short 2>&1 | tail -30
~~~

区别：同一条 pytest 命令逐字符一致，仅解说文本措辞不同，文本相似度为 0.76。首轮
续跑的 `A′(N+1)` 可见文本为空白但命令同样逐字复现；两轮续跑 verifier Reward 均为 1。

精确参数见
[`pvisor replay` 命令参考](../reference/cli.md#replay-an-agent-trajectory)；执行边界见
[执行指南](execution.md)。

## OpenCode 实验诊断（历史记录）

#### OpenCode 看门狗

OpenCode 是目前唯一既以黑盒 CLI 形式续跑、又缺乏可靠停止机制的 Agent：
Claude Code 的 `max_turns`、Codex 的 `agent_max_steps`、mini-swe-agent 的
`step_limit` 都在 resume 上原生生效，mini-swe 与 Pi 的循环本身由 pVisor 驱动；
而 OpenCode 两头都不占。看门狗由 `run_process` 的两个监督条件实现，仅对
OpenCode 续跑启用。

**为什么必须引入（两个实测问题）**

1. **resume 会话无视步数预算（上游缺陷）**。`agent.build.steps` 在全新会话上
   原生生效，但用 `opencode import` + `run --session` 恢复的会话完全忽略该
   配置。绕开 pVisor 的裸实验可直接复现：新会话设 `steps=2` 恰好 2 步停；
   resume 会话设 `steps=3` 连跑 32 步以上不停。pVisor 已把剩余预算
   （`max_steps - after_step`）写入隔离配置，但该值目前不被读取。贪心采样下
   模型还会陷入无限循环（实测单回合刷 24 万 token、本地循环 200+ 回合），
   若无外部干预，续跑会一直占用沙箱直到外层 agent 超时（小时级）。
2. **续跑进程偶发静默僵死**。实测抓到过：CLI 进程存活但无网络连接、无工具
   子进程、事件循环空转，既不推进也不退出。根因在 OpenCode/Bun 一侧，且不会
   自行恢复；同样会耗尽整个 agent 超时窗口。

**如何解决（两个看门狗的机制）**

| 看门狗 | 触发条件 | 动作 |
|---|---|---|
| 步数看门狗 | stderr 进度日志中的回合行数达到剩余预算 | SIGINT 优雅终止 |
| 空闲看门狗 | stdout/stderr 连续 10 分钟无任何输出（僵死特征） | 终止进程组 |

步数看门狗的信号源经过专门筛选。OpenCode 的 stdout JSONL 事件流被 Bun 运行时
按 ~8KB 块缓冲（管道、PTY、文件重定向均非实时），不能作为计数源；`--print-logs`
输出到 stderr 的进度日志实时流式，且每个模型回合固定产生一行
`message=loop ... step=N`。续跑命令因此固定附加 `--print-logs`，pVisor 实时统计
该行数，达到剩余预算即向进程组发 SIGINT——SIGINT 下 OpenCode 会优雅退出并刷出
缓冲的全部事件；若进程被更强信号杀死导致 stdout 缓冲丢失，则从 OpenCode 的
sqlite 会话库重建续跑事件（任务沙箱自带 python3，无需新增依赖），续跑轨迹不丢。

终止结果如实上报：步数看门狗触发时 `agent_status` 为 `max_steps`，结果 metadata
携带 `opencode_step_budget` 标记（`enforced_by: pvisor_event_watchdog`）。
隔离配置中的 `agent.build.steps` 仍然保留：一旦上游修复 resume 会话读取该配置，
原生预算将直接生效，看门狗自动退化为兜底保险。

