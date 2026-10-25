# pVisor VM：同次启动的 Ready 与 Exit

Benchmark: B-STARTUP ([注册表](../REGISTRY.md#b-startup)), role user-facing.

## Motivation

测量预准备环境从创建进程到首条正确输出的等待，并保留同一次启动的正常退出时间。
预准备 rootfs，2 vCPU / 128 MiB，独立 stage；验证真实 VM 隔离和空变更集。

## Experiment design

测量边界、准备流程、逐次成功判据和证据说明遵循
[公共协议与复现步骤](REPRODUCING.md)。下面的 TOML 定义本条件的实际命令与采样参数；
准备器解析它，再由 semaspec 调用 Ready/Exit 观察器。环境与制品要求见公共协议的输入表。

<!-- semspec: bench id=pvisor-vm -->
```toml
argv = [
  "@BINARY@",
  "spawn",
  "--no-agent-defaults",
  "--overlaynet",
  "off",
  "--stdio",
  "inherit",
  "--timeout",
  "30s",
  "--stage",
  "@BATCH@/current/stage",
  "--vm",
  "--rootfs",
  "@ASSETS@/rootfs",
  "--cpu",
  "2",
  "--memory",
  "128MiB",
  "--",
  "/bin/sh",
  "-c",
  "printf 'REFERENCE_READY\\nREFERENCE_RESULT {\"mode\":\"ready\",\"correctness\":\"passed\"}\\n'"
]
cwd = "@BATCH@/workspace"
warmup = 3
runs = 60
timeout = 30
[env]
PVISOR_RUN_HOME = "@BATCH@/current/runs"
PVISOR_STARTUP_TIMING = "0"
PVISOR_FS_PROFILE = "0"
```

## Data and analysis

```sh
: "${ASSETS:?set ASSETS to verified reference inputs}"
: "${BINARY:?set BINARY to the validated pvisor executable}"
out=benchmark/startup/.data/pvisor_vm_timing-repro-01
mise run benchmark -- startup/pvisor_vm_timing --assets "$ASSETS" --binary "$BINARY" --output "$out"
mise exec -- python3 benchmark/run.py --audit "$out"
```

输出目录须为新目录；保留证据后可单独审计。当前已验证范围见公共协议，未运行的后端不填充成绩。
