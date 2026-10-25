# 远程镜像冷暖客户端配对

Benchmark: B-LAZY-STARTUP ([注册表](../REGISTRY.md#b-lazy-startup)), role user-facing.

## Motivation

客户端第一次按需读取远程镜像及紧随其后的再次执行，分别要等待多久、传输了多少数据？服务端准备成本单列。

期望结论：同源应用下 OCI、TurboOCI 与 pVisor 的冷/暖客户端 Ready、Completion、传输量和配对区间；本机 loopback 协议不外推 WAN 或冷磁盘。

## Experiment design

使用独立准备器冻结制品、输入和随机顺序；3 次预热、30 轮随机配对。Python 行脚本拥有协议、Ready/Completion 时钟及统计，semaspec 只调用一次，不做外层采样。每个样本必须通过独立正确性校验；失败保留，不能从缺失输入生成性能结论。复现、资源限制和证据范围见 [startup 手册](README.md) 与 [TurboOCI 协议](TURBO_OCI.md)。

<!-- semspec: table id=image-lazy-compare -->
```json
{
  "cold-warm-client-paired": "$ python3 @HARNESS@/image_lazy_compare.py --measure @BATCH@"
}
```

## Data and analysis

需要已准备并核对身份的私有 OverlayBD lab、固定镜像来源、pVisor prepared store、
同次构建的 `pvisor`/`pvisor-daemon` 制品和不重叠的应用/服务 CPU。
这些输入不随 checkout 分发，本次没有验证其从零准备；来源与已知限制见
[公共复现说明](REPRODUCING.md) 和 [lab 手册](OVERLAYBD_LAB.md)。
采样轮次与随机种子仍由此规格调用的脚本固化，Markdown 不另存一份配置。

另需先前已验证的预准备镜像批次 `WARM_BATCH`，其中须有 `prepare/{oci,turbo}/result.json`；
此处复用其准备收据，不把其旧耗时并入本次冷/暖统计。

```sh
: "${LAB:?set LAB to a validated private lab under .data}"
: "${STORE:?set STORE to the verified prepared image store}"
: "${BINARY_DIR:?set BINARY_DIR to matching pvisor binaries}"
: "${CPUS:?set CPUS to available application CPUs}"
: "${SERVICE_CPUS:?set SERVICE_CPUS to disjoint service CPUs}"
: "${WARM_BATCH:?set WARM_BATCH to validated image_compare evidence}"
out="benchmark/startup/.data/image_lazy_compare-repro-01"
python3 benchmark/startup/image_lazy_compare.py --prepare "$out" \
  --lab "$LAB" --prepared-store "$STORE" --binary-dir "$BINARY_DIR" \
  --cpus "$CPUS" --service-cpus "$SERVICE_CPUS" --warm-batch "$WARM_BATCH"
mise run sspec -- bench --cwd "$out" "$out/in.json"
python3 "$out/harness/image_lazy_compare.py" --audit "$out"
```

输出目录必须为新目录。专用审计重算逐次输出、计时、输入身份与统计，并核对 semaspec 导出；
通用 `benchmark/run.py --audit` 仅用于基本 startup/其他通用批次，不替代此专用审核器。
审计成功后仍需独立发布审核；这里不自动更新历史成绩或批准语义。
