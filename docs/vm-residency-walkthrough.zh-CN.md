# pVisor pause / offload / resume：逐文件阅读指南

本次在 gpu02 的 `/home/zhaoyu/Programs/pvisor` 上修改，基线是 develop 的 `6d4d95b9d4dea0dec59166f5c0f3ad2602cb6173`。没有提交或推送。旁边的 `/home/zhaoyu/Programs/pvisor-pause-resume` 保持原样；其中已有的暂停恢复实现被移植到当前 executor 目录结构，内存回收部分为本次新增。

## 先理解这一版完成的事情

```text
pvisor pause JOB
  → 本地 control.sock → 管理进程 VmControl → runner 内部 socket
  → libkrun 主循环 → 全部 vCPU ACK → 设备活动屏障
  → Paused

pvisor offload JOB --mib 128
  → 管理进程验证 Paused 和独立 cgroup
  → 后台分批 memory.reclaim → status 显示实际回收量
  → VM 仍是 Paused

pvisor resume JOB
  → 确认没有正在进行的 reclaim
  → 打开设备活动屏障 → 全部 vCPU 恢复 ACK
  → Running；被换出的页面访问时由内核加载
```

这不是 snapshot/restore。runner、KVM 对象、文件描述符和设备对象一直存在。有效匿名页交给 Linux swap 保存；没有自定义快照文件，也不能在原进程退出后恢复。

这一版由调用者明确选择暂停时机。尚未实现推理/下载接管适配器、自动识别空闲阶段、多 VM 的恢复预算调度、预取或跨进程恢复。普通 TCP 连接和远端服务可能在 VM 暂停期间超时。

## 推荐阅读顺序

先看 ① `overlay.rs` 的消息类型，② `vm_control.rs` 的父进程协调，③ `supported.rs` 的进程与 cgroup 边界，④ libkrun `runtime_control.rs` 的暂停完成条件，⑤ `pause.rs` 的设备屏障，最后看 ⑥ `vm_memory.rs`。不要从所有设备文件同时开始。

## 1. 接口：从“只能查看和结束”到“可暂停、回收、恢复”

| 文件（相对项目根目录） | 原来的行为 | 修改后的行为 |
| --- | --- | --- |
| `crates/persisting-control/src/overlay.rs` | 本地控制协议主要处理运行查询和文件层操作 | 增加 `Pause/Resume/VmStatus/Offload`；定义执行状态、内存采样及卸载报告 |
| `crates/persisting-control/src/runtime.rs` | RunState 缺少暂停转换和故障状态 | 增加 `Pausing/Resuming/Faulted`，已确认暂停映射到原有 `Suspended` |
| `crates/persisting-pvisor/src/cli/mod.rs` | 不认识 pause/resume/offload 子命令 | 增加命令分发，并避免默认 run 参数改写吞掉新命令 |
| `crates/persisting-pvisor/src/cli/runtime.rs` | status 依赖持久记录和 live 标志 | 增加生命周期及异步回收命令；status 输出实时 VM 状态和内存报告；暂停时仍可 kill |
| `crates/persisting-pvisor/src/cli/run.rs` | VM 启动参数没有内存 cgroup 路径 | 增加 `--vm-cgroup-parent PATH`，传入 VM 配置 |
| `crates/persisting-pvisor/src/config.rs` | VmSettings 只配置 rootfs、固件、CPU 和内存等 | 增加可选 `vm.cgroup_parent`；默认不创建 cgroup、不启用 offload |

先看 `VmRuntimeState` 和 `VmOffloadState` 的区别：执行状态始终保持 Paused，内存回收任务可以是 reclaiming/completed/partial/failed/cancelled。不能把“请求了 128 MiB”理解为“已释放 128 MiB”。

## 2. 管理进程：状态、生命周期串行化与非阻塞回收

| 文件 | 原来的行为 | 修改后的行为 |
| --- | --- | --- |
| `crates/persisting-pvisor/src/runtime/vm_control.rs`（新增） | 没有运行期 VM 控制器 | 串行控制 socket；检查请求编号和 ACK；缓存状态；统计已确认暂停时长；接受后台回收任务，阻止恢复与回收并发 |
| `crates/persisting-pvisor/src/runtime/vm_memory.rs`（新增） | 没有每台 VM 的内存回收对象 | 创建独立 cgroup、采样、检查 swap、写 memory.reclaim、释放空 cgroup |
| `crates/persisting-pvisor/src/runtime/registry.rs` | 控制服务偏向 OverlayFS，部分运行没有控制 socket | 所有 live Job 都有控制入口；pause/resume 的等待移到独立处理线程；offload 接受后立即返回，status 仍可用 |
| `crates/persisting-pvisor/src/runtime/attempt.rs` | Attempt 持有 Gateway、OverlayFS、网络等资源 | 同时持有 VmControl，并把它交给 executor；任务暂停时继续持有运行租约 |
| `crates/persisting-pvisor/src/runtime/mod.rs` | 没有 VM 控制/内存模块 | 注册新模块及控制函数 |
| `crates/persisting-pvisor/src/executor/mod.rs` | AttemptAttachments 主要传递网络资源 | 同时传递同一个 VmControl，使 CLI 与 VM executor 观察同一台 VM |

`vm_control.rs` 值得逐个读的函数：

- `request()`：发送请求不等于完成；收到匹配编号、匹配状态的 ACK 才更新最终状态。超时后关闭旧连接，避免迟到 ACK 污染下一次操作。
- `offload()`：在与 pause/resume 相同的串行边界上检查 Paused，记录操作编号和基线，启动后台回收。
- `run_offload()`：每次最多请求 64 MiB，根据实际 `memory.current` 差值继续；连续无进展或超过软时间预算时返回 partial。`EAGAIN` 不是 VM 故障。
- `status()`：读取当前内存统计；不会等待 reclaim 的内核调用结束。
- `ActiveClock`：只扣除已经确认 Paused 的时间，启动和转换耗时仍计入 watchdog。

offload 的 30 秒是批次之间检查的软预算。内核中的一次 reclaim write 可能阻塞，不能承诺到时立即取消。任务终止后停止后续批次；runner 会先被终止和回收，管理进程随后等待当前回收调用返回、收拢线程并释放 cgroup。因此慢 I/O 可能延迟管理进程的最终退出，但不会推迟向 runner 发出终止。

## 3. executor：谁进入 cgroup、什么时候进入

文件：`crates/persisting-pvisor/src/executor/vm/supported.rs`。

原来：生成 RunnerSpec → 启动 runner → 等待退出/取消/超时。

现在：

1. 若配置 cgroup_parent，在该已委派父目录下创建唯一的 `pvisor-vm-*` 子 cgroup。
2. 父进程打开子 cgroup 的 `cgroup.procs`。runner 在 `pre_exec` 中通过 async-signal-safe `write(fd, "0", 1)` 把自己放进去，再 exec。guest RAM 此时尚未分配。
3. 原管理进程不会迁移。Gateway、OverlayNet 继续在管理进程侧工作。
4. 为 runner 继承控制 socket；网络和控制 fd 分别固定为 198/199。先把源 fd 复制到 200 以上，再覆盖目标，防止父进程打开很多文件时源 fd 恰好相撞。
5. libkrun 真正建好 VM 后回复 status，作为就绪确认。
6. executor 订阅状态，将暂停/恢复变化同步到 RunState，并保留独立的取消与 child.wait 路径。
7. 子进程退出并回收后，在 blocking worker 中等待 reclaim 线程结束，再释放内存管理对象。这避免管理进程正常退出时遗弃后台清理；强制 SIGKILL/宿主崩溃后的残留清理仍不在本版保证内。

没有 cgroup_parent 时 pause/resume 仍然可用；offload 会明确说明需在启动前配置。pVisor 不自动启用祖先控制器、不改 swap、不缩小 memory.max。

## 4. libkrun：从 vCPU 原语到 VM 级暂停

| 文件 | 原来的行为 | 修改后的行为 |
| --- | --- | --- |
| `vendor/krun-vmm/src/linux/vstate.rs` | 已有 Pause/Resume；重复 Pause 可能没有 ACK；x86 pvclock 有 TODO | 补齐重复请求确认、时钟暂停通知及控制断开处理；不宣称 guest 时间完全冻结 |
| `vendor/krun-vmm/src/lib.rs` | 主要在启动时恢复 vCPU | 增加运行时暂停/恢复协调：先广播给全部 vCPU，再在共享截止时间内收齐响应 |
| `vendor/libkrun/src/runtime_control.rs`（新增） | 没有运行期控制请求处理器 | 解析 JSONL、执行状态机；暂停顺序为 vCPU→设备，恢复顺序为设备→vCPU；失败进入 faulted |
| `vendor/libkrun/src/lib.rs` | `krun_start_enter()` 持续运行普通事件循环 | 增加控制 fd API；在完整事件批次结束后处理控制；暂停时只等待控制请求，不分发普通设备事件 |

为什么要等一个事件批次结束？如果在某个回调里直接宣告暂停成功，同一批次剩下的设备回调可能继续写 guest 内存。成功边界必须位于批次外部。

这些修改保留当前 develop 的 musl 嵌入式内核初始化路径，没有用旧版文件覆盖当前启动逻辑。旁边上游 libkrun 仓库不是 Cargo 实际链接的实现；真正修改的是上述 vendor 文件。

## 5. 设备：暂停主循环为什么还不够

新增 `vendor/krun-devices/src/virtio/pause.rs`：共享屏障包含 `paused` 和 `active`，worker 取得活动 guard 后才能操作设备；guard 必须覆盖到 used-ring 回写和中断通知结束。

```text
运行：enter → active += 1 → 处理操作及完成通知 → drop → active -= 1
暂停：关闭入口 → 等待 active == 0 → 完成确认
恢复：打开入口 → 唤醒等待的 worker
```

| 文件 | 原来的行为 | 加入屏障后的边界 |
| --- | --- | --- |
| `vendor/krun-devices/src/virtio/mod.rs` | 未导出暂停模块 | 注册 pause 模块 |
| `vendor/krun-devices/src/virtio/fs/worker.rs` | 独立 fs worker 可继续处理队列 | 每次队列事件获取 guard，覆盖文件操作和完成通知 |
| `vendor/krun-devices/src/virtio/net/worker.rs` | 网络 worker 独立运行 | 每次事件处理先取得 guard |
| `vendor/krun-devices/src/virtio/console/process_rx.rs` | RX descriptor 和输入等待混在循环中 | descriptor 处理受保护；等待宿主输入/空闲时释放 guard |
| `vendor/krun-devices/src/virtio/console/process_tx.rs` | TX worker 独立输出 | 输出和完成回写受保护；无 descriptor 时释放 guard 再 park |
| `vendor/krun-devices/src/virtio/vsock/muxer_thread.rs` | 独立处理 proxy 事件 | 事件处理取得 guard |
| `vendor/krun-devices/src/virtio/vsock/reaper.rs` | 独立回收过期 proxy | 回收取得 guard |
| `vendor/krun-devices/src/virtio/vsock/timesync.rs` | 独立向 guest 注入时间信息 | 注入取得 guard |

锁顺序：先 guard，后设备锁。空闲等待不能持有 guard。已经进入的宿主 I/O 如果长期阻塞，pause 可以超时，但不能把超时当成功。

这一屏障针对单 runner 单 VM 的 fs/net/console/vsock 配置。未审计的 GPU、block、sound、input、TEE 等功能组合会拒绝启用控制，不能默认继承暂停保证。

## 6. 内存结果如何读

`status --json` 中的 `vm_status.memory` 包含：

- `cgroup`：只有 runner 所在的内存组。
- `sample.current_bytes`：cgroup 当前内存，包含 guest RAM 之外的部分。
- `sample.swap_bytes`：cgroup 换出量。
- `sample.anon_bytes / file_bytes`：区分匿名页和文件缓存。
- `last_offload`：操作编号、目标量、前后采样、耗时、完成/部分完成/失败/取消状态。

驻留减少量看 `before.current_bytes - after.current_bytes`，并结合 swap 增量判断；不要仅看进程 RSS，也不要把文件缓存下降全部称作 guest RAM 卸载。cgroup 数值是采样，不是精确同步的内存审计。

恢复 ACK 只表示执行重新开放，换入延迟还会影响 guest 的第一个有效操作。因此真实测试分别记录 `resume_ack_ms` 和 `first_progress_ms`。

## 7. 使用方法

管理员或启动服务需先提供已委派 memory controller 的 cgroup v2 父目录，并配置可用 swap。下面的 CGROUP_PARENT 指这样的目录；不是任意磁盘目录。

```bash
cd /home/zhaoyu/Programs/pvisor
./target/debug/pvisor run --executor vm --vm-cgroup-parent "$CGROUP_PARENT" ...
./target/debug/pvisor pause "$JOB"
./target/debug/pvisor offload "$JOB" --mib 128
./target/debug/pvisor status "$JOB" --json
# 确认 last_offload.state 不再是 reclaiming 后：
./target/debug/pvisor resume "$JOB"
```

也可在 TOML 的 `[vm]` 下填写 `cgroup_parent = "/sys/fs/cgroup/..."`。它是启动配置，不能事后将已分配的 guest RAM 简单归账到新组。

## 8. 新增与迁入的验证文件

| 文件 | 检查内容 |
| --- | --- |
| `scripts/test-libkrun-pause-resume.py` | 迁入并适配当前 CLI；真实 KVM、重复暂停恢复、30 秒暂停、原 PID 和文件句柄延续、暂停时终止 |
| `scripts/test-vm-memory-offload.py` | 新增；在独立委派测试 scope 内验证 runner 隔离、真实 swap、驻留下降、恢复进展、回收期间终止及清理 |
| `scripts/fixtures/vm-memory-probe.c` | 新增；guest 填充 256 MiB 确定性数据，保持打开的文件，恢复后访问页面，退出前完整校验 |

真实测试只使用自己的临时 rootfs、工作目录、VM 和委派 scope；没有修改系统 swap 或祖先内存限制。测试内的文件停止标记用于结束测试，不代表外部文件托管的一致性协议已实现。

## 9. 查看原文件和修改后文件

项目内 `target/validation/residency/review/` 保存本轮基线的 before、修改后的 after，以及包含新增文件的 `changes.patch`。基线文件来自上述 commit；新增文件只有 after。完整 `git diff` 之外，这份补丁也包含尚未 tracked 的新文件。

```bash
git diff -- crates/persisting-pvisor/src/executor/vm/supported.rs
git diff -- vendor/krun-vmm/src/linux/vstate.rs
less target/validation/residency/review/changes.patch
```

先读本指南解释，再看对应 diff；遇到新增模块，可以先读上述关键函数，再读错误处理和测试。

## 10. 本轮验证结果

2026-10-07 在 gpu02 / Linux x86_64 上实际执行：

| 验证 | 结果 | 项目内日志 |
| --- | --- | --- |
| `just test control pvisor` | 最终 341 通过，5 跳过 | `target/validation/residency/tests-final.log` |
| libkrun runtime_control 测试 | 6 通过，2 未选中 | `target/validation/residency/libkrun-tests.log` |
| 设备屏障独立测试 | 3 通过 | `target/validation/residency/device-tests.log` |
| Debug build / Clippy `-D warnings` | 通过 | `build-final.log` / `clippy-final.log`（同目录） |
| containerd shim，启用 vm feature | 编译通过 | `target/validation/residency/shim-check.log` |
| fmt、diff 空白、Python 语法、C 严格语法检查 | 通过 | 对应命令退出码 0 |
| KVM，网络关闭，含 30 秒暂停与暂停时终止 | 通过 | `target/validation/residency/kvm-pause.log` |
| KVM，启用 OverlayNet，重复暂停恢复及终止 | 通过 | `target/validation/residency/kvm-network-final.log` |
| KVM，真实内存换出，2 轮 | 通过；256 MiB 完整数据校验、原 PID/计数/文件句柄延续 | `target/validation/residency/kvm-offload-final.log` |
| KVM，回收期间终止 | 退出码 130，cgroup 清理通过 | `target/validation/residency/kvm-cancel-offload-final.log` |

最终回收测试的 guest RAM 配置是 768 MiB，guest 实际填充 256 MiB 数据。第一次请求 128 MiB，cgroup `memory.current` 从 337,219,584 B 降至 202,919,936 B，swap 从 0 增至 134,475,776 B。回收耗时 374 ms；resume ACK 约 31.6 ms，guest 首次进展约 683 ms。第二轮回收耗时 413 ms，首次进展约 1,286 ms。首次进展按 50 ms 轮询观察，不能当作高精度微基准。

这些是功能性实验，证明页面可换出且恢复后数据正确，不是最大内存节省率、尾延迟或高并发密度基准。OverlayNet 测试只证明该设备配置可以暂停恢复，不保证任意远端 TCP 连接跨长暂停存活。

中间一次完整测试因临时端口 `Address already in use` 失败；该项单独重跑和最终完整重跑均通过，原失败日志保存在 `tests-port-conflict.log`。没有为通过测试修改网络隔离逻辑。

当前没有验证 macOS/aarch64 的这项新功能，也没有更改原有 AppArmor 策略。测试复用了已安装的 `pvisor-dev` profile。

真实回收测试可按下面方式复现；每次使用唯一的临时 scope 名称：

```bash
systemd-run --user --scope --unit="pvisor-residency-test-$(date +%s)" \
  -p Delegate=yes --quiet aa-exec -p pvisor-dev -- \
  python3 scripts/test-vm-memory-offload.py \
  --firmware-dir /home/zhaoyu/Programs/pvisor-pause-resume/target/libkrunfw/5.5.0-x86_64-unknown-linux-gnu
```

脚本只在这个 scope 内把自身放入 supervisor 子组，并给 scope 开启 memory controller；项目运行不自动修改祖先 cgroup。可加 `--cancel-after-offload` 复现回收期间终止。设备屏障模块的独立 runner 是 `rustc --edition 2021 --test vendor/krun-devices/src/virtio/pause.rs`，因为该 patched dependency 不属于 workspace，直接对它跑 Cargo 测试受依赖布局限制。
