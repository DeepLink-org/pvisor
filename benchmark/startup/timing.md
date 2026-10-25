# 八种预准备执行环境的 Ready / Exit 对照

Benchmark: B-STARTUP ([注册表](../REGISTRY.md#b-startup)), role user-facing.

## Motivation

Agent 频繁创建一次性环境时，启动等待直接决定交互延迟和短任务的总成本。用户需要知道 pVisor 各模式的启动处于什么量级，以及与 Docker 和轻量 VM 相比如何。

期望结论："已准备好环境时，pVisor host/staged 的首条命令可在 X ms 内执行，VM 在 Y ms 内执行，与 Docker、Firecracker 处于同一量级或差多少"；以及"完整发行版的启动成本为 Z 秒级，pVisor 无镜像 VM 能省掉多少"。

## Experiment design

每行执行对应 `*_timing.md` 中的唯一计划；3 次预热、60 次正式样本。逐行独立采样，不推导随机配对加速比例。每个条件的实际 argv、超时、环境和正确性范围见相应规格。

- [Native](native_timing.md)
- [pVisor host](pvisor_host_timing.md)
- [pVisor staged](pvisor_staged_timing.md)
- [pVisor VM](pvisor_vm_timing.md)
- [Docker](docker_timing.md)
- [Firecracker](fc_system_timing.md)
- [QEMU](qemu_timing.md)
- [QEMU microvm](qemu_microvm_timing.md)

<!-- semspec: table id=timing -->
```json
{
  "Native": "$ python3 @REPO@/benchmark/startup/timing.py @BATCH@ --backend native",
  "pVisor host": "$ python3 @REPO@/benchmark/startup/timing.py @BATCH@ --backend pvisor-host",
  "pVisor staged": "$ python3 @REPO@/benchmark/startup/timing.py @BATCH@ --backend pvisor-staged",
  "pVisor VM": "$ python3 @REPO@/benchmark/startup/timing.py @BATCH@ --backend pvisor-vm",
  "Docker": "$ python3 @REPO@/benchmark/startup/timing.py @BATCH@ --backend docker",
  "Firecracker": "$ python3 @REPO@/benchmark/startup/timing.py @BATCH@ --backend fc-system",
  "QEMU": "$ python3 @REPO@/benchmark/startup/timing.py @BATCH@ --backend qemu",
  "QEMU microvm": "$ python3 @REPO@/benchmark/startup/timing.py @BATCH@ --backend qemu-microvm"
}
```

## Data and analysis

先按 [公共复现步骤](REPRODUCING.md) 验证不需要外部资产的 Native：

```sh
out=benchmark/startup/.data/timing-native-repro-01
mise run benchmark -- startup/timing --row Native --output "$out"
mise exec -- python3 benchmark/run.py --audit "$out"
```

整表需要八个后端的全部输入与运行环境，当前尚未验证干净环境整表复现。
`run.json` 记录本次实际选择的行，不把单行通过视为整表完成。
读已有结果只使用 `semaspec filter`；已发布数据不随此规格更新。
