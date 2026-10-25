# 沙箱生命周期管理

Agent 在模型推理与工具执行之间交替。等待模型响应时，沙箱可能几乎不使用 CPU，却仍保留任务所需的内存。pVisor 将运行、暂停、卸载与恢复纳入生命周期管理，使执行环境能够保留上下文，同时按任务进展调整资源驻留。

## Host 如何参与管理 {#host-management}

在 VM 路径中，OverlayCore 和 OverlayNet 将文件与网络交互接到 Host，运行时负责 vCPU 与设备执行。Host 因而能在受支持的路径上观察 I/O 进展，并通过运行时接点协调暂停与恢复；部分后端 I/O 可以在 guest 停驻期间由 Host 继续推进。

能看到等待请求，还不足以判断整台 VM 可以停驻。自动管理还需要 guest 空闲信息、设备内存访问协调及可靠的唤醒条件，避免暂停其他仍需运行的工作。自动等待管理的实现阶段与验证范围见 [EXP-001](../design/experimental/vcpu-idle-offload/index.md)；文件与网络接管本身不表示所有等待路径都已启用自动卸载。

## 暂停、卸载与恢复 {#live-lifecycle}

| 操作 | 对执行与资源的影响 | 继续执行的条件 |
| --- | --- | --- |
| pause | 暂停 vCPU；guest RAM 通常仍驻留 | 恢复原实例的 vCPU 执行 |
| offload | 在 CPU 与设备满足静默条件后，将 RAM 同步到后备存储并请求回收驻留页 | 原 VMM 和可靠的 RAM 恢复来源仍保留 |
| resume | 恢复原实例的执行与设备访问 | 已卸载页面可能在后续访问时缺页读回 |

暂停提供执行协调点，卸载才进一步降低闲置 RAM 的驻留。卸载和恢复会消耗 I/O、CPU 与时间，是否值得执行取决于等待长度、内存规模和恢复成本；短等待不一定适合卸载。具体机制见[内存卸载](../design/memory-optimization/offload.md)。

## 与检查点和任务结束的区别 {#checkpoint-boundary}

上述运行时操作继续同一个存活实例。原生 Job 不带 live VM 标志的 `suspend` 则发布完整执行检查点，并等待原 VM 退出；对已停止且保存了执行检查点的 Job，`resume` 创建新的 Attempt，保留 Job 身份。执行检查点需要符合指定的原生 VM 配置，支持范围见 [CLI 参考](../reference/cli.md#full-vm-execution-checkpoints)。

结束执行后，暂存的文件结果仍可审查；接受或丢弃文件结果属于另一项决定。检查点、分支与不同状态的复用方式见[状态保存与分支复用](resource-reuse.md)。

## 按接口核对支持范围 {#availability}

运行时具备某个操作，不代表所有接入接口都暴露它。单节点 daemon 提供同一 Attempt 的 pause/resume，目前未提供 offload、检查点或 stage/apply API。原生 Job、Rust Runtime 与 daemon 的范围见[接入方式矩阵](../guides/infrastructure/integration.md#capabilities)。

生命周期管理通过回收等待期的驻留资源，为并发任务留出空间；实际容量与延迟收益仍需结合工作负载和节点总内存测量，见[VM 内存基准](../benchmarks/vm-memory/index.md)。

## CLI 示例：暂停、卸载与恢复 {#cli-example}

需要先了解调用方如何提交任务与收集结果时，使用[基础设施快速开始](../start/infrastructure.md)。下面的示例单独验证 live VM 控制与内存连续性。

### 1. 准备服务器与镜像 {#prerequisites}

在同一台 Linux x86_64 服务器上打开两个终端。先完成[安装](../start/installation.md)，确认当前账号能读写 `/dev/kvm`，且系统允许 pVisor 创建 user namespace。检查 CLI 选项：

```bash
pvisor --version
pvisor suspend --help
pvisor resume --help
```

帮助中应有 `--vm-pause`、`--vm-offload` 和 `--vm-load`。若报 `setgroups` 或 `uid_map` 权限错误，请管理员检查 user namespace 与 AppArmor 策略；KVM 组权限不能替代这些条件。AppArmor 策略需匹配实际 pVisor 可执行文件路径。

准备独立的 **Linux amd64 rootfs**，包含 `/usr/local/bin/python3`。演示使用从 `python:3.12-slim` 解包的文件系统；固定镜像摘要可复现环境。服务器无法连接镜像仓库时，可在联网机器准备相同架构的 rootfs，保留权限和符号链接传入服务器。不要使用宿主 `/`。准备方式见 [VM 执行器](../guides/executors/vm.md)。

在有足够空间的私有目录中执行，替换 `ROOTFS` 为实际绝对路径：

```bash
mkdir -p pvisor-vm-demo/workspace
cd pvisor-vm-demo/workspace
export ROOTFS=/absolute/path/to/linux-rootfs
```

每次使用新的演示目录，rootfs 放在该目录之外。配置为 1 个 vCPU、256 MiB guest RAM，Host 侧另有内存开销。

### 2. 启动任务 {#start-task}

在第一个终端创建脚本：保存 8 MiB 校验数据，持续输出计数；调用方创建 `finish` 文件后，检查内存并写入结果。这个文件仅用作演示中的结束信号。

```bash
cat > task.py <<'PY'
import hashlib
import json
import pathlib
import time

memory = bytearray(bytes(range(256)) * 32768)
expected = hashlib.sha256(memory).hexdigest()
counter = 0
print('ready', flush=True)
while not pathlib.Path('finish').exists():
    assert hashlib.sha256(memory).hexdigest() == expected
    counter += 1
    print(f'progress {counter}', flush=True)
    time.sleep(1)
assert hashlib.sha256(memory).hexdigest() == expected
result = {'memory_ok': True, 'steps': counter}
pathlib.Path('result.json').write_text(json.dumps(result) + '\n')
print(json.dumps(result), flush=True)
PY
```

启动 VM，保持第一个终端运行：

```bash
pvisor spawn --executor vm --rootfs "$ROOTFS" \
  --overlaynet off --memory 256MiB --cpu 1 --timeout 5m \
  --vm-ram-backing "$PWD/../guest.ram" \
  --stage ../stage --mount "$PWD:stage" \
  -- /usr/local/bin/python3 -u task.py
```

**预期结果：**出现 `ready` 和递增的 `progress N`。`task.py` 使用工作区相对路径，不依赖固定的 guest 工作目录。`--overlaynet off` 关闭 guest 网络；不要叠加要求网络控制的 `--safe`。

### 3. 暂停并继续 {#pause-resume}

第二个终端进入同一 `pvisor-vm-demo/workspace`，使用同一个 pVisor 可执行文件。保存执行身份，再暂停：

```bash
pvisor status ../stage --json > ../before.json
pvisor suspend ../stage --vm-pause > ../pause.json
```

**预期结果：**`pause.json` 的 `value.vm.state` 为 `paused`；已有输出排空后，计数停止。Pause 暂停 vCPU，内存通常仍驻留。

```bash
pvisor resume ../stage --vm-load > ../pause-resume.json
```

**预期结果：**`pause-resume.json` 的 `value.vm.state` 为 `running`，计数从暂停处继续。

### 4. 卸载内存并恢复 {#offload-resume}

```bash
pvisor suspend ../stage --vm-offload > ../offload.json
```

**预期结果：**`offload.json` 的 `value.vm.state` 为 `offloaded`，`value.vm.memory` 包含 backing 路径和驻留页观测。Offload 建立 CPU／设备静默边界，将 RAM 同步到 backing 并请求回收驻留页。

```bash
pvisor resume ../stage --vm-load > ../offload-resume.json
touch finish
```

恢复后计数继续；`finish` 使任务检查内存、写入 `result.json` 并退出。等待第一个终端的命令结束，再收集结果。

以上操作继续**同一个 VM Attempt**，依赖原 owner／VMM 存活；普通 `suspend` 则保存执行检查点，语义见[与检查点的区别](#checkpoint-boundary)。演示使用普通文件 backing，不启用压缩、去重或共享池。驻留页观测不等于整个服务的内存占用，也不保证回收量。

### 5. 收集结果 {#collect-results}

在第二个终端执行：

```bash
pvisor status --review --json ../stage > ../result.bundle.json
pvisor inspect ../stage -- cat result.json > ../result.json
python3 - <<'PY'
import json
from pathlib import Path

root = Path('..')
before = json.loads((root / 'before.json').read_text())
bundle = json.loads((root / 'result.bundle.json').read_text())
result = json.loads((root / 'result.json').read_text())
assert bundle['run']['state'] == 'completed'
assert bundle['run']['exit_code'] == 0
assert bundle['run']['attempt_id'] == before['run']['attempt_id']
assert result['memory_ok'] is True and result['steps'] > 0
print('PASS: same Attempt, memory intact, task completed')
PY
```

**预期结果：**输出 `PASS: same Attempt, memory intact, task completed`。`memory_ok` 为 `true`，`steps` 随操作时间变化。调用方可将自己的任务 ID 与 Bundle 中的 Job／Attempt ID 关联，再进行评分或调度。

任务结束后 VM 已退出。保留需要的记录与结果，再丢弃未合入的候选：

```bash
pvisor drop ../stage
```

显式指定的 `guest.ram` 是调用方保存的 backing 文件，确认 VM 已退出且不再需要后自行清理。中途终止用 `pvisor kill ../stage`；5 分钟任务超时在暂停期间仍计时。

此流程于 2026-10-24 在 Linux x86_64/KVM、CLI `0.3.0+g35.b318715` 上验证：pause／offload 后计数停止，恢复后继续，Attempt 不变，内存校验通过且退出码为 0。这是单 VM 功能验证，不是并发密度或自动模型等待管理测试。
