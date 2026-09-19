# Persisting benchmarks

**仓库级基准入口：pVisor 进程启动与 Run Bundle 访问。**

脚本只拥有可复现的测量和报告契约，不拥有被测组件的产品行为。

## pVisor

```bash
just benchmark-pvisor
just benchmark-pvisor nightly target/pvisor-benchmark/nightly
just benchmark-pvisor-compare \
  target/pvisor-benchmark/candidate/raw-report.json \
  target/pvisor-benchmark/main/raw-report.json
```

详见 [`pvisor/`](pvisor/README.md)。

## Links

- [pVisor design](../docs/src/en/pvisor/design/index.md)
