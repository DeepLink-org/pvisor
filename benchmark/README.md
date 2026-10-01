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

## Links

- [pVisor design](../docs/src/zh/design/index.md)
