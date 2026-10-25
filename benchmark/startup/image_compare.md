# 同源镜像的预准备启动配对

Benchmark: B-LAZY-STARTUP ([注册表](../REGISTRY.md#b-lazy-startup)), role user-facing.

## Motivation

相同镜像内容已经准备好时，比较普通 OCI、TurboOCI 和 pVisor rootfs VM 的首条正确输出等待；镜像准备与客户端冷缓存是独立问题。

期望结论：同源 Ubuntu shell 在三种完整产品路径上的 Ready/Completion 分布、配对差及失败。结果包含 Docker exec 客户端等路径差异，不解释为纯 VMM 或镜像格式成本。

## Experiment design

使用独立准备器冻结制品、输入和随机顺序；3 次预热、30 轮随机配对。Python 行脚本拥有协议、Ready/Completion 时钟及统计，semaspec 只调用一次，不做外层采样。每个样本必须通过独立正确性校验；失败保留，不能从缺失输入生成性能结论。复现、资源限制和证据范围见 [startup 手册](README.md) 与 [TurboOCI 协议](TURBO_OCI.md)。

<!-- semspec: table id=image-compare -->
```json
{
  "warm-prepared-paired": "$ python3 @HARNESS@/image_compare.py --measure @BATCH@"
}
```

## Data and analysis

需要已准备并核对身份的私有 OverlayBD lab、固定镜像来源、pVisor prepared store、
同次构建的 `pvisor`/`pvisor-daemon` 制品和不重叠的应用/服务 CPU。
这些输入不随 checkout 分发，本次没有验证其从零准备；来源与已知限制见
[公共复现说明](REPRODUCING.md) 和 [lab 手册](OVERLAYBD_LAB.md)。
采样轮次与随机种子仍由此规格调用的脚本固化，Markdown 不另存一份配置。

```sh
: "${LAB:?set LAB to a validated private lab under .data}"
: "${STORE:?set STORE to the verified prepared image store}"
: "${BINARY_DIR:?set BINARY_DIR to matching pvisor binaries}"
: "${CPUS:?set CPUS to available application CPUs}"
: "${SERVICE_CPUS:?set SERVICE_CPUS to disjoint service CPUs}"
out="benchmark/startup/.data/image_compare-repro-01"
python3 benchmark/startup/image_compare.py --prepare "$out" \
  --lab "$LAB" --prepared-store "$STORE" --binary-dir "$BINARY_DIR" \
  --cpus "$CPUS" --service-cpus "$SERVICE_CPUS"
mise run sspec -- bench --cwd "$out" "$out/in.json"
python3 "$out/harness/image_compare.py" --audit "$out"
```

输出目录必须为新目录。专用审计重算逐次输出、计时、输入身份与统计，并核对 semaspec 导出；
通用 `benchmark/run.py --audit` 仅用于基本 startup/其他通用批次，不替代此专用审核器。
审计成功后仍需独立发布审核；这里不自动更新历史成绩或批准语义。
