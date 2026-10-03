# 退出码与错误

脚本应当先保留退出码，再读取 Run Bundle。退出码告诉你命令是否成功，记录里的失败类型帮助你区分任务失败、准备失败和策略拒绝。

任务命令可能自己返回 `1`、`2` 或 `125`，所以不能只凭一个数字决定是否重试、换执行器或评价 Agent。

## 当前退出行为

| 情况 | CLI 行为 |
| --- | --- |
| 被运行命令正常结束 | `run` 返回该命令退出码；成功通常为 0 |
| 实际经过时间超时 | 1；`run.failure.kind = "deadline_exceeded"`；没有正常任务退出码 |
| Run 取消 | 返回 130 |
| 执行失败但没有命令退出码 | 返回 1 |
| 内部 host sandbox 安装失败 | 启动器使用 125；外层错误也可能以 1 返回，需结合诊断 |
| 参数解析错误 | clap 返回 2 |
| `status` / `apply` / `drop` / `kill` 成功 | 返回 0 |
| 上述命令发生解析后的运行错误 | `anyhow` 错误返回 1，stderr 说明原因 |
| `inspect` 中的命令结束 | 返回该检查命令的退出码 |
| 伴随工具 | 派发保留其退出码；replay 成功与质量还要查看结果协议 |

应用冲突、Job 未找到与 UnsupportedPolicy 目前没有独占的数字退出码。工作负载自身也可能返回 1、2、125 或 130，所以数字不能单独区分“Agent 失败”和“pVisor 拒绝”。

## 在自动化里判断

保存 stderr 和执行返回值，然后按明确的 stage 路径查找 Run Bundle。Bundle 中 `run.state`、`run.exit_code` 和 `run.failure` 提供执行结果；准入或准备阶段失败可能尚无完整 Bundle，应记录为基础设施/启动失败，不能当成“无改动的成功”。

不要因 Agent 返回 0 自动 apply。apply 有独立的冲突与恢复路径，其返回值也必须检查。超时、取消和非零退出都可能留下可审查改动；流程见[CI](../guides/ci.md)。

## 保留失败记录的 shell 写法 {#shell}

```bash
set +e
pvisor run --safe --overlaynet-deny-all --stdio capture \
  --stage ../stage-exit-001 -- /bin/sh -c 'printf "candidate\n" > result.txt; exit 7'
run_code=$?
pvisor status --review --json ../stage-exit-001 > ../stage-exit-001.review.json
review_code=$?
printf 'run=%s review=%s\n' "$run_code" "$review_code"
```

命令返回 `7`，仍可能有可评审的 `result.txt`。分别保存运行与读取记录的状态；CI 在收集完产物后用原始 `run_code` 结束步骤。确认修改适用再 apply，决定放弃则 drop。

## 按记录的失败类型处理 {#failures}

`run.failure` 包含 `kind`、`message`、`retryable`。把 `retryable` 当作执行器的提示，再结合任务决定是否重试。超时是没有正常进程退出码的失败执行，与命令自行返回 `1` 不同。

| Failure kind | 含义 / 处理 |
| --- | --- |
| `invalid_spec` | 已解析请求无效；先修正配置再重试 |
| `unsupported` | 执行器无法满足请求的能力；调整请求或执行器 |
| `spawn` | 任务或 VM 启动失败；检查路径、程序与设备诊断 |
| `process_exit` | 任务返回非零状态；保留其 `exit_code` |
| `workload` | 执行器报告了普通退出状态以外的任务失败 |
| `deadline_exceeded` | 任务超过实际经过时间限制；检查进度与保留文件 |
| `infrastructure` | sandbox 设置或 I/O 等执行基础设施失败；检查诊断 |

解析错误和准入前失败可能发生在 `run.failure` 或 Bundle 生成之前。找不到 Job、apply 冲突和管理命令错误写到 stderr 并返回 1，不会伪装成任务失败。`UnsupportedPolicy` 诊断表示能力请求被拒绝；普通 execution checkpoint/suspend 请求使用 `CAPABILITY_UNSUPPORTED`。不要把这些情况评分为模型回答失败。

可下载的[非零退出样例](../../assets/examples/json/failed-run.json) 记录 `state = "failed"`、`exit_code = 7`、`failure.kind = "process_exit"`。[超时样例](../../assets/examples/json/timeout-run.json) 记录 `state = "failed"`，省略 `exit_code`，`failure.kind = "deadline_exceeded"`，CLI 返回 1。两者都保留了暂存的候选文件。[采集来源](../../assets/examples/json/provenance.json) 包含命令与实际退出码。
