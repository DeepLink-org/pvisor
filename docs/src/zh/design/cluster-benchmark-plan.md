# 已退役的 Cluster benchmark 计划

**Controller/Worker 实验计划已退役，不转移给 daemon。** B-CLUSTER 没有活动 runner 或验收协议。[历史测量](../benchmarks/cluster-scalability.md)只回答冻结制品的问题，不能验证下列提案或当前 daemon 密度。

## 已有证据 {#existing}

[技术归档](cluster-performance-analysis.md)区分增配预算的就绪探针、合成历史查询和独立完整任务批次。就绪不是完成吞吐；fixture RSS 不是控制面的净内存；热回放不是完整恢复。历史表格、CSV 和原始 `.data/` 保留各自批次，不改名、不新增测量。

## 启动归因提案 {#q1}

直接 VM 执行与旧 Cluster 分发的对照是未完成的归因提案，不是当前 Controller/Worker 实验。旧 debug、release 和不同配额的配置不能相减推导调度成本。

## 边际内存提案 {#q2}

区分共享基底、私有工作集与 offload 恢复仍是测量问题，不是已证实的 Cluster 密度收益。独立 B-DENSITY 和 B-VM-MEMORY 探针按各自 registry 与 receipt 继续保留。单 VM 回收内存不能换算成 daemon 容量。

## 固定预算有效工作提案 {#q3}

模型等待/释放 CPU 的 A/B 提案没有被 shell 就绪或独立 Python/Git 完成批次验证。退役的 `release_cpu_on_idle` 路径不是活动测量对象，不能据此给出当前有效 Agent 吞吐或单位成本结论。

## 历史扩展提案 {#q4}

归档计数调用与保留记录回放不能证明 HTTP 调度吞吐、大活跃集合行为或当前 daemon 历史成本。旧 scheduler example 没有活动测量入口。

## 恢复提案 {#q5}

旧 Controller/Worker 对账实验已退役。本地热日志回放不能证明端到端恢复、分区收敛或 exactly-once 执行。退役实验不弱化任何安全 claim 或 receipt。

## 持续运行提案 {#q6}

短探针不能证明长期资源有界。原常驻 Worker 计划不是当前 daemon 可靠性或保留策略测试。

## 替代实验必须满足的条件 {#protocol}

替代实验需要独立登记的对象与用户问题、冻结源码/二进制/输入身份、核验全进程树 CPU/内存/零 swap 控制、相同负载、预先声明的样本数和停止规则，以及完整正确性、冲突与恢复检查。保留失败、未知和 OOM。独立批次与批次内相关观察分开；A/B 结论报告配对不确定性。正式采样前冻结协议，不得从探索数据事后批准假设。

这些条件不授权新增测量或语义审批。人工审阅的 claim、receipt、semspec 检查与批准记录均不变。

## 当前范围 {#priority}

没有可执行的 Cluster 优先级列表或复现命令。[本机容量](../benchmarks/density.md)和[VM 内存](../benchmarks/vm-memory/index.md)仅在各自 native 探针范围内使用。daemon 密度、多主机扩展和相同条件调度系统对照均未测。[运行手册](https://github.com/DeepLink-org/pvisor/blob/main/benchmark/pvisor/README.md)列出退役入口与独立活动探针。
