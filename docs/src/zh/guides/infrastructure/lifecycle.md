# 暂停、卸载与恢复 VM {#cli-example}

需要先了解调用方如何提交任务与收集结果时，使用[基础设施快速开始](../../start/infrastructure.md)。下面的示例单独验证 live VM 控制与内存连续性。

## 1. 准备服务器与镜像 {#prerequisites}

在同一台 Linux x86_64 服务器上打开两个终端。先完成[安装](../../start/installation.md)，确认当前账号能读写 `/dev/kvm`，且系统允许 pVisor 创建 user namespace。检查 CLI 选项：

```bash
pvisor --version
pvisor suspend --help
pvisor resume --help
```

帮助中应有 `--vm-pause`、`--vm-offload` 和 `--vm-load`。若报 `setgroups` 或 `uid_map` 权限错误，请管理员检查 user namespace 与 AppArmor 策略；KVM 组权限不能替代这些条件。AppArmor 策略需匹配实际 pVisor 可执行文件路径。

准备独立的 **Linux amd64 rootfs**，包含 `/usr/local/bin/python3`。演示使用从 `python:3.12-slim` 解包的文件系统；固定镜像摘要可复现环境。服务器无法连接镜像仓库时，可在联网机器准备相同架构的 rootfs，保留权限和符号链接传入服务器。不要使用宿主 `/`。准备方式见 [VM 执行器](../executors/vm.md)。

在有足够空间的私有目录中执行，替换 `ROOTFS` 为实际绝对路径：

```bash
mkdir -p pvisor-vm-demo/workspace
cd pvisor-vm-demo/workspace
export ROOTFS=/absolute/path/to/linux-rootfs
```

每次使用新的演示目录，rootfs 放在该目录之外。配置为 1 个 vCPU、256 MiB guest RAM，Host 侧另有内存开销。

## 2. 启动任务 {#start-task}

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

## 3. 暂停并继续 {#pause-resume}

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

## 4. 卸载内存并恢复 {#offload-resume}

```bash
pvisor suspend ../stage --vm-offload > ../offload.json
```

**预期结果：**`offload.json` 的 `value.vm.state` 为 `offloaded`，`value.vm.memory` 包含 backing 路径和驻留页观测。Offload 建立 CPU／设备静默边界，将 RAM 同步到 backing 并请求回收驻留页。

```bash
pvisor resume ../stage --vm-load > ../offload-resume.json
touch finish
```

恢复后计数继续；`finish` 使任务检查内存、写入 `result.json` 并退出。等待第一个终端的命令结束，再收集结果。

以上操作继续**同一个 VM Execution**，依赖原 owner／VMM 存活；普通 `suspend` 则保存执行检查点，语义见[与检查点的区别](../../concepts/lifecycle.md#checkpoint-boundary)。演示使用普通文件 backing，不启用压缩、去重或共享池。驻留页观测不等于整个服务的内存占用，也不保证回收量。

## 5. 收集结果 {#collect-results}

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
assert bundle['run']['execution_id'] == before['run']['execution_id']
assert result['memory_ok'] is True and result['steps'] > 0
print('PASS: same Execution, memory intact, task completed')
PY
```

**预期结果：**输出 `PASS: same Execution, memory intact, task completed`。`memory_ok` 为 `true`，`steps` 随操作时间变化。调用方可将自己的任务 ID 与 Bundle 中的 Session／Execution ID 关联，再进行评分或调度。

任务结束后 VM 已退出。保留需要的记录与结果，再丢弃未合入的候选：

```bash
pvisor drop ../stage
```

显式指定的 `guest.ram` 是调用方保存的 backing 文件，确认 VM 已退出且不再需要后自行清理。中途终止用 `pvisor kill ../stage`；5 分钟任务超时在暂停期间仍计时。

此流程于 2026-10-24 在 Linux x86_64/KVM、CLI `0.3.0+g35.b318715` 上验证：pause／offload 后计数停止，恢复后继续，Execution 不变，内存校验通过且退出码为 0。这是单 VM 功能验证，不是并发密度或自动模型等待管理测试。

## 管理容器 VM 内存 {#container-memory}

在 Linux x86_64/KVM 上，用 `pvisor run --branchable --network none` 启动后台
源容器，不添加挂载、端口发布、重启策略或健康检查。在 guest 命令空闲时运行
`pvisor container branch SOURCE --offload --names a b`。子分支共用一份密封的
RAM 基底，各自保留私有写入与文件系统状态。回收内存需要磁盘支撑的容器存储。
子分支 `a` 中的命令结束后，运行 `pvisor container offload a` 并等待成功，
在发送下一条 guest 命令前运行 `pvisor container resume a`。源容器删除后子分支仍可
运行，子分支无法用这些文件冷启动。
[容器分叉契约](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor/README.md#fork-a-running-oci-session-on-one-host)

对普通 Linux x86_64 OCI 容器 VM，`pvisor run --managed-memory` 选用原始文件
支撑的 RAM，并提供 offload/resume 控制。调用方可将 VM 显式登记到
`pvisor-daemon pressure-serve`，使用委派的 cgroup v2 子树。调用方报告安全等待点，
并在派发工作前等待许可就绪。等待期间禁止直接 exec 或在 guest 中继续后台工作。
服务仅管理已登记的 VM，并保留原 Execution 身份。OpenSandbox/E2B SDK 的
生命周期调用有独立入口。
[压力调度与 owner 协议](https://github.com/DeepLink-org/pvisor/blob/main/crates/pvisor-daemon/README.md#opt-in-pressure-scheduling-for-ordinary-oci-executions)
