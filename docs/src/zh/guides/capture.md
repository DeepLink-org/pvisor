# 捕获 Agent 轨迹

Gateway capture 是 pVisor 的 Run 驱动，由 Run 启停；系统不再提供独立 Gateway 命令或
守护进程。[Capability 与 Evidence 模型](../concepts/capabilities-and-evidence.md)解释
Capture 能证明什么，以及它不负责 enforce 什么。

先按[安装指南](../start/installation.md)安装 `pvisor`；本页直接使用已安装的命令。真实 Agent 可直接通过 `pvisor run` 配置：

```bash
export DEEPSEEK_API_KEY=sk-...
pvisor run \
  --name deepseek \
  --gateway-mode capture \
  --gateway-route 'name="deepseek", upstream="https://api.deepseek.com/v1", api_key_env="DEEPSEEK_API_KEY"' \
  --gateway-route 'name="*", forward="deepseek"' \
  -- claude
```

pVisor 启动内嵌 Gateway、向子进程注入代理或 base URL、等待执行、排空捕获并停止
Gateway。使用 `--record-destination ./capture` 将 Trace Event journal 写入指定目录。
`--gateway-stream-markdown` 仅保留兼容参数，当前不生成 Markdown 投影。

### 事件时间戳与顺序

默认输出为 `events.trace.jsonl`：先写 `pvisor.trace/4` 文件头，再写包含 Event 与
`{journal, offset}` 的记录。观测时间为 `event.observed_at_unix_ms`，跨生产者因果使用
`event.caused_by`，不能从时间戳推导。

Gateway 内容位于 `event.data.payload.content`，story/session 路由位于
`event.data.payload.story`，call 关联位于 `event.data.payload.correlation`。
Run 与内嵌 Gateway 共用 Journal。进入队列不等于持久化；同步文件后才返回 LocalSync。
只支持正式 Event Journal，旧 JSONL 不再兼容读取。

客户端只有使用注入的代理或 base URL 才能被观察；直接 socket 是否受限取决于 executor，
实际隔离边界以 Run Bundle 为准。

下一步：阅读 [Gateway 内部实现](../design/gateway.md)。
