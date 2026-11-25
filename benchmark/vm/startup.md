# VM 启动时间专报

Benchmark: B-VM-STARTUP-ENG ([注册表](../REGISTRY.md#b-vm-startup-eng))，engineering A/B。
PR Conversation 的 **VM startup performance** 和 Actions 的 **VM startup regression gate**
展示独立的表格，不向 [15 项 guest 工作负载](guest.md) 添加行。每格是基线 → 候选，
单位为毫秒（Criterion 中位估计），附变化率。

| 启动方式 | pVisor VM + Linux 固件 | pVisor VM + Rust native | Firecracker |
| --- | --- | --- | --- |
| 直接启动 | 新 VM | 新 VM | 新 VM，参照 |
| daemon 预热启动 | 命中一个已停泊的 VM | N/A：不支持延迟挂载根文件系统 | N/A：不由 pvisor-daemon 管理 |

**直接启动（Create → VMM Running）**：从宿主发起 VMM spawn 前，到宿主读到该 VMM 的成功启动事件。
pVisor 启用启动追踪，取当前 runner 在首次 vCPU resume 成功后发出的 `vm.start_worker` 成功记录；
不能用仍处于暂停状态的 `on_ready`。Firecracker 取固定版本主线程确认已启动 microVM 的日志。
使用宿主单调时钟记录完整行到达的时间，不相减两套时钟。随后静态 guest 探针必须确认文件系统类型、
恰好输出一次 `VM_STARTUP_READY` 并正常退出；这些是样本验收条件，不是停表点。
pVisor 用 virtio-fs，Firecracker 从 ext4/virtio-blk 启动，并以探针为 PID 1。

**预热启动（Create → sandbox Running）**：从客户端发出 HTTP Create，到完整读完 202 响应；
只接受 `status.state == "Running"`。不计入后续首条命令、文件校验、删除、daemon 启动和补池。
复用 [daemon warm pool 的校验](../criterion/src/startup/warm_pool.rs)：请求前记录已停泊槽位，
要求分配相同 ID、命中时钟有效、命令正确读写私有文件、删除后返回 404 且释放 cgroup。
池空回退不能替代命中。两行分别是 VM 层 Running 和沙箱 API 层 Running，不能直接相减来推断预热节省量。

**门禁与环境**：同机、4 vCPU / 256 MiB，宿主绑定同四个 CPU；每格 10 个 Flat 统计样本。
两个 pVisor 直接启动项和 Linux 预热项的 Create → Running 均检查 15% 阈值，只有 Firecracker
作参照。受支持项测量失败、超时或缺基线均不放行；旧首输出/首命令基线不能复用。
CI 在两个版本间复用同一预先准备的固定 Ubuntu 镜像，不把下载、daemon 启动或补池计入延迟。

## 本地复现（Linux x86_64 / KVM）

按 [guest 门禁准备说明](guest.md#本地复现) 准备两版 VM API 运行器、Linux 固件及固定 Firecracker 和 ELF 内核。
另需两版 `pvisor-daemon`（带 warm pool），动态构建须将对应的 Linux 固件放在其旁并命名为 `libkrunfw.so.5`。
从候选 checkout 执行，使用有 cpu、memory、pids 委派的 cgroup v2；`4-7` 替换为四个可用 CPU：

```bash
set -euo pipefail
mkdir -p benchmark/vm/.data
batch="$(mktemp -d "$PWD/benchmark/vm/.data/startup.XXXXXX")"
export PVISOR_BENCH_GUEST="$batch/probe"
cc -O2 -static -Wall -Wextra -Werror benchmark/vm/startup_probe.c -o "$PVISOR_BENCH_GUEST"
export PVISOR_BENCH_DAEMON_IMAGE=ubuntu PVISOR_BENCH_DAEMON_IMAGES="$batch/images"
"$CANDIDATE_DAEMON" prepare-image --name ubuntu --images-dir "$batch/images" \
  --image mirror.gcr.io/library/ubuntu@sha256:9a04f13b3f80ef056361e30868593b1cfd2279fe19e4081cb93a90bf56819d6a
export PVISOR_BENCH_FIRECRACKER=/prepared/firecracker PVISOR_BENCH_FC_KERNEL=/prepared/vmlinux
mise exec -- cargo build --release --locked --manifest-path benchmark/criterion/Cargo.toml --bin pvisor-benchmarks
measure() {
  systemd-run --user --scope --quiet --unit="pvisor-warm-$1-$$" \
    -p Delegate=yes -p CPUQuota=400% -p MemoryMax=2G -p MemorySwapMax=0 \
    env PVISOR_BENCH_LINUX_FW="$3" PVISOR_BENCH_DAEMON="$4" taskset -c 4-7 \
    "$PWD/target/release/pvisor-benchmarks" vm/startup --binary "$2" --output "$batch/$1" \
    --criterion-output "$batch/criterion" -- --save-baseline "$1"
}
measure main "$BASE_VM" "$BASE_FW" "$BASE_DAEMON"
measure candidate "$CANDIDATE_VM" "$CANDIDATE_FW" "$CANDIDATE_DAEMON"
mise run benchmark-compare -- "$batch/criterion" --suite vm-startup --output "$batch/comparison"
```

比较超阈值时仍保留 `comparison.json`；用 `python3 -I scripts/ci/vm_guest_comment.py --startup-summary "$batch/comparison/comparison.json"`
预览 PR 表格，不发布评论。逐次 `create-to-running.json` 记录直接启动时钟，
`warm-hits.jsonl` 中的 `sample.create_ns` 是预热时钟；`first_command_ns` 仅作诊断。
