# Cluster VM 与模型 Gateway

用新的会话目录验证隔离与模型接入。离线 VM 快照控制和网络 Gateway 使用不同 profile；先停止上一套会话，保持最多两个执行并发。

## 真实 VM：准备与完整验证 {#vm}

已验证目标是 Linux x86-64 glibc 源码构建，需可读写 `/dev/kvm`、`/dev/fuse`、`ldd`，并提供含 `libkrunfw.so.5` 的固定固件目录。静态 musl 内嵌内核路径与 macOS HVF 不属于这个脚本的验证范围。

```bash
test -r /dev/kvm && test -w /dev/kvm
test -r /dev/fuse && test -w /dev/fuse
QS_FIRMWARE=$(python3 - <<'PY'
import sys
sys.path.insert(0, "scripts/packaging")
from stage_wheel_binaries import BuildOptions, _firmware_source
print(_firmware_source(BuildOptions())[0].parent)
PY
)
test -f "$QS_FIRMWARE/libkrunfw.so.5"
python3 scripts/cluster-quickstart.py verify --backend vm \
  --firmware-dir "$QS_FIRMWARE" --state "$QS_ROOT/verify-vm"
cat "$QS_ROOT/verify-vm/report.json"
```

固件准备命令复用项目打包逻辑：优先读取 `PVISOR_LIBKRUNFW_PATH`，否则使用本地缓存或下载固定版本、校验 archive SHA-256 后提取；首次需要 GitHub 网络。离线时预先提供正确固件目录，不要跳过文件检查。当前离线检查点注册要求显式绑定目录；打包逻辑在 `scripts/packaging/stage_wheel_binaries.py`，原生加载器在 `crates/pvisor-vm/src/firmware_store.rs`。

脚本只复制 host 的 `/bin/sh`、`/bin/sleep` 和 `ldd` 找到的动态库，构造小 rootfs；它不拉取大型镜像或把整个宿主 `/` 用作 lower。rootfs 同时配置为只读 lower，每个 Attempt 拥有独立 upper，保证 ELF 可运行、产物可导出和检查点可封存。每个 VM 128 MiB/1 vCPU，每个单槽位 Worker 仍有 512 MiB/0.5 核的全进程树硬限制。

预期完整基础流程加 `native VM pause/offload/resume/suspend and sealed-fork restore` 通过。恢复来自真实原生快照，并通过恢复后暂停 ACK 验证运行；没有使用模拟 VM 结果。

## 手工暂停、offload、封存与分叉 {#controls}

```bash
QS_STATE="$QS_ROOT/manual-vm"
python3 scripts/cluster-quickstart.py prepare --backend vm \
  --firmware-dir "$QS_FIRMWARE" --state "$QS_STATE"
source "$QS_STATE/env.sh"
trap 'python3 scripts/cluster-quickstart.py stop --state "$QS_STATE"' EXIT
python3 scripts/cluster-quickstart.py start --state "$QS_STATE"
"$QS_BIN/pvisor-cluster" submit "$QS_STATE/inputs/vm-controls.json"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task vm-controls --phase running
"$QS_BIN/pvisor-cluster" control vm-controls pause --request-id pause-1
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task vm-controls --phase paused
"$QS_BIN/pvisor-cluster" control vm-controls offload --request-id offload-1
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task vm-controls --phase offloaded
"$QS_BIN/pvisor-cluster" control vm-controls resume --request-id resume-1
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task vm-controls --phase running
"$QS_BIN/pvisor-cluster" control vm-controls suspend --request-id suspend-1
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task vm-controls --phase suspended
"$QS_BIN/pvisor-cluster" fork vm-controls "$QS_STATE/inputs/fork.json"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task vm-child --phase running
"$QS_BIN/pvisor-cluster" control vm-child pause --request-id child-pause
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task vm-child --phase paused
"$QS_BIN/pvisor-cluster" cancel vm-child
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task vm-child
python3 scripts/cluster-quickstart.py limits --state "$QS_STATE"
python3 scripts/cluster-quickstart.py stop --state "$QS_STATE"
trap - EXIT
```

每一步等待原生观察，再发下一动作。Pause/offload 保留 RAM 与槽位，只释放逻辑 CPU；resume 重新准入；suspend 封存完整状态并停止源，终态才释放全部预留。分叉例子只创建一个 child，源已经 suspended，因此不会并行再开源 VM。

控制样例有 240 秒 sleep 和 300 秒墙钟超时，为手工操作留出时间；内存、CPU 硬限制不变。停下来阅读过久可能任务先结束，此时用新的会话重试，不能复用终态 ID。相同 request ID 的重复控制/分叉幂等；不同内容不能共用 ID。分叉回执只证明创建，恢复后原生 ACK 才验证可执行性。跨主机迁移与 live fork 多分支不属于这条上手验证。

## 离线 Gateway：不需要供应商账户 {#gateway}

使用包含 Gateway feature 的构建，在真实 Attempt Gateway 前后运行实际 HTTP 请求。额外模型服务只在 loopback 提供确定性回复，有 128 MiB/0.1 核硬限制；它不调用外部模型，不测量模型质量或供应商性能。

```bash
python3 scripts/cluster-quickstart.py verify --backend host --gateway \
  --state "$QS_ROOT/verify-gateway"
cat "$QS_ROOT/verify-gateway/report.json"
```

预期 `Attempt Gateway model request, forbidden model and credential isolation` 通过：允许的请求收到回复，未经授权模型返回 403 且没有到达 upstream；Agent 只有本地 placeholder credential，不继承 Controller/Worker/供应商 token。Bundle 与混合原生/模型 trace 实际交付下载。

手工提交同一任务：

```bash
QS_STATE="$QS_ROOT/manual-gateway"
python3 scripts/cluster-quickstart.py prepare --backend host --gateway --state "$QS_STATE"
source "$QS_STATE/env.sh"
trap 'python3 scripts/cluster-quickstart.py stop --state "$QS_STATE"' EXIT
python3 scripts/cluster-quickstart.py start --state "$QS_STATE"
"$QS_BIN/pvisor-cluster" submit "$QS_STATE/inputs/gateway-agent.json"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task gateway-agent
"$QS_BIN/pvisor-cluster" artifacts gateway-agent --out "$QS_STATE/downloads/manual-agent"
python3 scripts/cluster-quickstart.py stop --state "$QS_STATE"
trap - EXIT
```

预期 stdout 为 `Gateway request passed; unauthorized model denied; credentials isolated`。`worker.toml` 中的 `gateway.routes` 负责路由；任务的 `gateway.models` 要求节点能力，`run.capabilities.models` 才授予访问。模型匹配和 capture level 必须一致。

## 接入正式 Agent 与模型服务 {#production}

在 Worker profile 把 `gateway.routes[].upstream` 换为服务的 API base，`api_key_env` 指向 Worker 服务环境中的供应商密钥；禁止 inline `api_key`。更新 TaskSpec 的具体模型要求、能力和 Agent 命令，用新 Task/Run ID 提交，先检查网络策略与后端隔离。接入命令和供应商环境变量见[Agent 接入](../agents/index.md)。

启用真实模型需要账户、网络和供应商合同，上面的离线验证不覆盖它们。VM 推理等待还需网络 profile、`release_cpu_on_idle` 与整个 guest 空闲声明，不能在离线快照 profile 上直接启用；完整顺序见[推理等待设计](../../design/cluster/lifecycle.md#inference)。

多主机部署需固定 rootfs/环境输入、可达 URL、TLS、节点存储和检查点兼容性。复制本机目录不自动建立这些合同；当前通过性证据与资源观测见[验证记录](index.md#verify)。
