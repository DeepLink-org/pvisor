# 相同 staged 执行器的惰性加载配对

Benchmark: B-LAZY-STARTUP ([注册表](../REGISTRY.md#b-lazy-startup)), role user-facing.

## Motivation

同一个 pVisor host staged 执行器读取相同镜像文件时，比较文件索引 FUSE 和 TurboOCI/OverlayBD 两种按需 lower 的准备、传输与应用完成成本。

期望结论：相同四文件、2,575,738 字节下的冷/暖客户端 Ready/Completion 与传输量，保留两种挂载路径的差异；不声称 host executor 已支持 OCI 程序执行。

## Experiment design

使用独立准备器冻结制品、输入和随机顺序；3 次预热、30 轮随机配对。Python 行脚本拥有协议、Ready/Completion 时钟及统计，semaspec 只调用一次，不做外层采样。每个样本必须通过独立正确性校验；失败保留，不能从缺失输入生成性能结论。复现、资源限制和证据范围见 [startup 手册](README.md) 与 [TurboOCI 协议](TURBO_OCI.md)。

<!-- semspec: table id=stage-lazy-compare -->
```json
{
  "same-stage-cold-warm-paired": "$ python3 @HARNESS@/stage_lazy_compare.py --measure @BATCH@"
}
```

## Data and analysis

需要已准备并核对身份的私有 OverlayBD lab、固定镜像来源、pVisor prepared store、
同次构建的 `pvisor`/`pvisor-daemon` 制品和不重叠的应用/服务 CPU。
这些输入不随 checkout 分发，本次没有验证其从零准备；来源与已知限制见
[公共复现说明](REPRODUCING.md) 和 [lab 手册](OVERLAYBD_LAB.md)。
采样轮次与随机种子仍由此规格调用的脚本固化，Markdown 不另存一份配置。

`RUNTIME_RLIB` 必须匹配 CLI 制品及准备收据。输出必须直接位于 `LAB` 下，名称不超过36字节。
`--preflight` 也消耗一个独立批次；通过后须重新准备另一个新目录用于正式测量，
不能删除 `measurement-started.json` 来重用预检目录。

```sh
: "${LAB:?set LAB to a validated private lab under .data}"
: "${STORE:?set STORE to the verified prepared image store}"
: "${BINARY_DIR:?set BINARY_DIR to matching pvisor binaries}"
: "${CPUS:?set CPUS to available application CPUs}"
: "${SERVICE_CPUS:?set SERVICE_CPUS to disjoint service CPUs}"
: "${RUNTIME_RLIB:?set RUNTIME_RLIB to the matching runtime library}"
out="$LAB/stage-repro-01"
python3 benchmark/startup/stage_lazy_compare.py --prepare "$out" \
  --lab "$LAB" --prepared-store "$STORE" --binary-dir "$BINARY_DIR" \
  --cpus "$CPUS" --service-cpus "$SERVICE_CPUS" --runtime-rlib "$RUNTIME_RLIB"
mise run sspec -- bench --cwd "$out" "$out/in.json"
python3 "$out/harness/stage_lazy_compare.py" --audit "$out"
```

输出目录必须为新目录。专用审计重算逐次输出、计时、输入身份与统计，并核对 semaspec 导出；
通用 `benchmark/run.py --audit` 仅用于基本 startup/其他通用批次，不替代此专用审核器。
审计成功后仍需独立发布审核；这里不自动更新历史成绩或批准语义。
