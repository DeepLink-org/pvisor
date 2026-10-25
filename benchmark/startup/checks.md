# Startup：可执行的测量与审计契约

Benchmark: B-STARTUP ([注册表](../REGISTRY.md#b-startup)), role user-facing.

## Motivation

相同启动事件的 Ready 与正常 Exit、逐次正确性检查和完整证据必须一起保留。
这些检查验证测量工具的行为，原生路径的测试结果不替代其他七种后端的实测。

## Experiment design

每个检查在 semaspec 提供的独立工作区执行。仓库源码和构建好的 semaspec 是输入；
测试不需要 Docker、KVM、rootfs 或网络，不安装工具、不操作已有服务、不发布用户数据。
公开测量仍按各个 `*_timing.md` 的参数运行，缺失准备输入必须拒绝。

<!-- semspec: setup -->
```bash
STARTUP_REPO=$(git -C "$SEMSPEC_PROJECT_ROOT" rev-parse --show-toplevel)
export STARTUP_REPO
export PYTHONPATH="$STARTUP_REPO/benchmark/tests:$STARTUP_REPO/benchmark:$STARTUP_REPO/benchmark/startup:$STARTUP_REPO${PYTHONPATH:+:$PYTHONPATH}"
```

### 输出、准备、超时与审计

**语义**：重复/错误/乱序 Ready、panic、错误退出、工作区污染、丢失的 trial 和
被篡改的统计必须被拒绝；预热与全部正式样本保留且从原始时钟重算。

**违反示例**：首条输出正确就忽略后续失败，或直接信任 render.json 的数字。

<!-- semspec: case id=S-STARTUP-001 timeout=30s -->
```bash
python3 -m unittest test_startup.StartupContracts test_startup.MarkdownPreparationContracts test_ready.ReadyContracts
```

### 原生启动完整链路

**语义**：从 Markdown 准备、经 semaspec 执行和独立审计后，原生启动保留
1 次预检、3 次预热与60次正式记录，Ready/Exit 同次配对；源文档保持不变。

**违反示例**：准备器绕过 Markdown 中的 argv，或执行后没有 semaspec 导出与64次证据。

<!-- semspec: case id=S-STARTUP-002 timeout=60s -->
```bash
if ! python3 -c 'import os; assert hasattr(os, "sched_getaffinity")'; then
  printf 'SKIP: Linux CPU affinity is required\n'
  exit 77
fi
cp "$STARTUP_REPO/benchmark/startup/native_timing.md" "$WS/source-before.md"
output="$WS/.data/native"
python3 "$STARTUP_REPO/benchmark/run.py" startup/native_timing --output "$output"
python3 "$STARTUP_REPO/benchmark/run.py" --audit "$output"
batch=$(python3 - "$output" <<'PYTHON'
import json, sys
from pathlib import Path
root = Path(sys.argv[1])
record = json.loads((root / "run.json").read_text())
assert record["status"] == "passed" and record["selected_rows"] == ["Native"]
assert len(record["batches"]) == 1
print(root / record["batches"][0]["directory"])
PYTHON
)
cmp "$WS/source-before.md" "$STARTUP_REPO/benchmark/startup/native_timing.md"
python3 - "$batch" <<'PY'
import json, sys
from pathlib import Path
from update import validate_batch
batch = Path(sys.argv[1])
manifest, rows, results = validate_batch(batch)
assert manifest["runs"] == 60 and manifest["warmup"] == 3
assert len(list((batch / "trials").iterdir())) == 64
assert rows["Native"]["N"] == 60
assert rows["Native"]["Ready median ms"] <= rows["Native"]["Exit median ms"]
assert json.loads((batch / "in.bench.json").read_text())["commands"][0]["kind"] == "script-row"
assert set(results) == {"native"}
PY
```

### 未准备的入口

**语义**：未准备的 startup 规格拒绝测量，不创建批次或覆盖结果。

**违反示例**：把 null 行当作成功测量，或用历史数字填充缺失输入。

<!-- semspec: case id=S-STARTUP-003 timeout=30s -->
```bash
set +e
python3 "$STARTUP_REPO/benchmark/run.py" startup/lazy_startup \
  --output "$WS/.data/unprepared" >"$WS/stdout" 2>"$WS/stderr"
status=$?
set -e
[ "$status" -eq 1 ]
grep -Fq 'no prepared single-condition command' "$WS/stderr"
[ ! -e "$WS/.data/unprepared" ]
```

### 阶段诊断的时钟与失败

**语义**：宿主和 guest 的 process-relative epoch 分开解释，重叠 span 不相加；
只有明确 owner 绑定才计算跨进程交接。缺失、失败、重复 Ready 与超时保留为失败证据。

**违反示例**：将不同 PID 的相对时钟对齐，或把缺失阶段填成零后报告启动收益。

<!-- semspec: case id=S-STARTUP-004 timeout=30s -->
```bash
python3 -m unittest test_startup_profile.StartupProfileContracts
```

### 整次实验的完整性

**语义**：保留的整次选择只能在全部选中行通过后完成；独立审计拒绝漏行、失败、
路径越界和变化的子 manifest，不启动被测进程或发布数据。

**违反示例**：首行成功、第二行失败后，仅枚举成功目录并报告整个实验完成。

<!-- semspec: case id=S-STARTUP-005 timeout=30s -->
```bash
python3 -m unittest test_run.TableRunnerContracts test_run.RetainedStartupContracts
```

## Data and analysis

```sh
mise run sspec -- lint benchmark/startup
mise run sspec -- run benchmark/startup/checks.md --require-pass
```

PASS 是本次检查的观察，与人工审核状态独立；这些新规格保持未审核。
原生端到端检查的纳秒样本仅为测试证据，不作为用户性能数字。
