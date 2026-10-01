# PolicyVisor benchmarks

**仓库级基准入口：pVisor 启动、资源占用与 Run Bundle 访问。**

脚本只拥有可复现的测量和报告契约，不拥有被测组件的产品行为。

## pVisor

```bash
just benchmark
just benchmark nightly target/pvisor-benchmark/nightly
just benchmark-compare \
  target/pvisor-benchmark/candidate/raw-report.json \
  target/pvisor-benchmark/main/raw-report.json
```

详见 [`pvisor/`](pvisor/README.md)。

三种隔离级别、进程选项及第三方实现的启动与资源占用对比，也见
[`pvisor/`](pvisor/README.md#sandbox-startup-and-resource-occupancy)。

## Replay 实验

[Qwen3.6 SandboxReplay 实验记录](replay/qwen3.6-results.md)保留历史样本和逐题比较，
与当前用户指南分开维护。报告列出尚缺的复现信息，不作为产品保证。

## Links

- [pVisor design](../docs/src/zh/design/index.md)
