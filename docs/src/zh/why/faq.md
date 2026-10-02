# 常见问题

**和 Agent 自带的沙箱有什么不同？**
自带沙箱解决「能不能挡住」。pVisor 在此之上提供事后选择性合入、冲突保护和可核对的执行记录，并且跨 Agent、跨执行器保持一致语义。见[对比](comparisons.md)。

**会不会拖慢 Agent？**
pVisor 增加的是准入、文件拦截和记录的开销。启动与文件系统开销的实测见[基准与对比](../benchmarks/index.md)（部分建设中）。

**能在 CI 里跑吗？**
可以。今天就能按 L1 方式在流水线里用：让 Agent 跑完、审查、只合入想要的。策略与证据驱动的免审和集群化（L2/L3）是方向，见[在 CI 中运行 Agent（规划中）](../guides/ci.md)。

**支持哪些 Agent？**
任何命令都能跑；对 Claude Code、Codex 等提供按可执行文件名匹配的预设。见[接入你的 Agent](../guides/agents/index.md)。

**数据会离开本机吗？**
pVisor 自己不收集、也不上传使用数据。但 Agent 调用模型 API 的流量本来就会出网，pVisor 不会让它留在本机：`--safe` 下只放行该 Agent 的模型 API 白名单，其余出口按策略处理。只有你显式启用 Gateway 捕获或使用远程镜像缓存服务时，才会引入 pVisor 自己的网络行为。见[安全概览](../security/index.md)。

**`last` 为什么找不到我刚跑的 Job？**
用 `--stage PATH` 时 Job 存在暂存目录里，`last` 只在默认存储中按工作区查找。把路径或 Job ID 显式传进去即可。见[Job 与存储](../concepts/jobs.md)。

**暂存能撤销外部副作用吗？**
不能。暂存只覆盖文件；外部 API 调用、数据库写入、已发出的消息不会因此回滚。见[暂存与 apply 语义](../concepts/staging.md)。
