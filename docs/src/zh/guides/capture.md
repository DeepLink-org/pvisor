# 捕获 Agent 轨迹

要检查 Agent 发给模型的请求和收到的响应，可以为 Job 启用 Gateway capture。它记录经过 Gateway 的模型流量，随 Run 启停，无需单独启动 Gateway 命令或守护进程。捕获范围与强制控制的区别见[能力与证据](../concepts/capabilities-and-evidence.md)。

先按[安装指南](../start/installation.md)安装 `pvisor`。真实 Agent 可直接通过 `pvisor spawn` 配置：

```bash
export DEEPSEEK_API_KEY=sk-...
pvisor spawn \
  --name deepseek \
  --gateway-mode capture \
  --gateway-route 'name="deepseek", upstream="https://api.deepseek.com/v1", api_key_env="DEEPSEEK_API_KEY"' \
  --gateway-route 'name="*", forward="deepseek"' \
  -- claude
```

pVisor 启动内嵌 Gateway、向子进程注入代理或 base URL、等待执行、排空捕获并停止
Gateway。使用 `--record-destination ./capture` 将 Trace Event journal 写入指定目录。

### 事件时间戳与顺序

默认输出为 `events.trace.jsonl`：先写 `pvisor.trace/5` 文件头，再写包含 Event 与
`{journal, offset}` 的记录。观测时间为 `event.observed_at_unix_ms`，跨生产者因果使用
`event.caused_by`，不能从时间戳推导。

Gateway 内容位于 `event.data.payload.content`，story/session 路由位于
`event.data.payload.story`，call 关联位于 `event.data.payload.correlation`。
Run 与内嵌 Gateway 共用 Journal。进入队列不等于持久化；同步文件后才返回 LocalSync。
只支持正式 Event Journal，旧 JSONL 不再兼容读取。

客户端只有使用注入的代理或 base URL 才能被观察；直接 socket 是否受限取决于 executor，
实际隔离边界以 Run Bundle 为准。

Gateway Journal 用于核对模型流量与事件关系。[从历史边界继续执行](replay.md)使用的是 Agent 自身保存的原生轨迹；需要回放时，也要保留所选适配器要求的原生会话文件。
