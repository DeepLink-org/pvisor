# Cluster 恢复与存储操作

使用[首个任务指南](index.md#start)已启动的手工会话，保留同一 `QS_STATE`、`QS_BIN` 和环境变量。先完成证据下载，再做会令远端证据不可下载的退休操作。

## Drain 与重新准入 {#drain}

```bash
"$QS_BIN/pvisor-cluster" drain qs-a
"$QS_BIN/pvisor-cluster" submit "$QS_STATE/inputs/drained.json"
"$QS_BIN/pvisor-cluster" show drained
"$QS_BIN/pvisor-cluster" drain qs-a --resume
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task drained
```

drained 的标签要求节点 a，所以 drain 期间应为 `queued`；恢复准入后为 `succeeded`。Drain 阻止新预留，已经运行的任务和交付继续；它不是停止进程或卸载节点的替代指令。

## 重启 Controller 而不重新执行 {#restart}

```bash
"$QS_BIN/pvisor-cluster" submit "$QS_STATE/inputs/restart-me.json"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task restart-me --phase running
"$QS_BIN/pvisor-cluster" show restart-me > "$QS_STATE/before-restart.json"
python3 scripts/cluster-quickstart.py restart-controller --state "$QS_STATE"
touch "$QS_STATE/workspace/release"
python3 scripts/cluster-quickstart.py wait --state "$QS_STATE" --task restart-me > "$QS_STATE/after-restart.json"
python3 - "$QS_STATE" <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
before = json.loads((root / "before-restart.json").read_text())
after = json.loads((root / "after-restart.json").read_text())
assert before["lease"]["key"] == after["lease"]["key"]
assert after["phase"] == "succeeded"
assert (root / "workspace/once.txt").read_text() == "once\n"
print("same lease; one native execution; succeeded")
PY
```

这段使用 host 样例，命令先写一次标记，再等待 release 文件。重启保留日志和产物目录，Worker 继续运行；原 Worker poll 清除 pending 并重建 deadline。相同完整 key 和单次标记证明没有重执行。手工查询可能错过很短的 pending 窗口；一键验证会短暂停住 Worker 主循环来确定性检查这个状态。

不要停 Worker 再用新 incarnation 接管未知执行，也不要删除日志来“重置”。故障超过 Worker 的本地 watchdog 后，应检查原生终止和 outbox，不能假设续租能复活旧运行。

## 永久失联时怎么办 {#lost}

只有任务处于 `reconciliation_pending` 且原 Worker 确实不可恢复时，才使用显式 loss resolution。它不是正常上手步骤，也不会自动重跑。

从 `show` 结果提取完整 `lease.key`，包含 task、Worker、incarnation、generation；核对节点进程与外部副作用后，通过 `pvisor-cluster resolve-lost key.json` 结束身份。未知结果为 `lost`，已知原生结果保留；替代工作用新的 Task/Run/Attempt ID。完整合同见[失联处理](../../design/cluster/state-and-recovery.md#resolve-lost)。

## 存储策略、退休与 GC {#storage}

```bash
"$QS_BIN/pvisor-cluster" artifact-storage
"$QS_BIN/pvisor-cluster" artifact-storage --limits "$QS_STATE/artifact-limits.json"
RETIRE_BEFORE=$(python3 -c 'import time; print(int(time.time()*1000)+1)')
"$QS_BIN/pvisor-cluster" artifact-gc --retire-before-ms "$RETIRE_BEFORE" > "$QS_STATE/gc-plan.json"
python3 - "$QS_STATE/gc-plan.json" <<'PY'
import json, pathlib, sys
plan = json.loads(pathlib.Path(sys.argv[1]).read_text())
print("plan:", plan["id"])
print("retire:", [entry["task_id"] for entry in plan["retire"]])
print("objects:", len(plan["objects"]), "bytes:", plan["bytes"])
PY
PLAN_ID=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["id"])' "$QS_STATE/gc-plan.json")
"$QS_BIN/pvisor-cluster" artifact-gc --apply "$PLAN_ID"
"$QS_BIN/pvisor-cluster" show hello
```

执行 apply 前检查计划：该时间界限会退休此前完成的终态证据，包括 hello。apply 后 hello 的任务元数据/原生结果仍保留，远端 artifacts 请求应返回 410；先前下载的文件不受影响。计划 5 分钟有效，重启后需重新预览；活动引用、共享引用和下载保护仍有效的对象不删除。任何 pending 租约会阻止破坏性 GC。

这不清理 Worker 本地任务、spool、镜像缓存或快照，也不缩短 metadata log。首次意图/终态提交因 metadata quota 满额失败时，需要检查实际磁盘并提高日志容量，不能靠 artifact GC 解决。首次任务路径的一键验证包含在线策略、退休和 GC 的实际通过检查。

## 定位问题 {#troubleshooting}

| 现象 | 检查 |
| --- | --- |
| 用户 systemd 连接失败 | manager、会话 bus 与 cgroup delegation；脚本不会改为无限制运行 |
| 任务一直 queued | Worker 后端、标签、模型/环境能力、资源、租户配额与 drain |
| VM spawn failed | 明确固件目录、KVM/FUSE 权限、rootfs 中的 ELF 与动态库 |
| 原生成功但 task failed | `artifact_error`、要求的 upper/trace、上传配额和 outbox |
| 重启后长期 pending | 原 Worker 是否仍在、完整活动清单、incarnation；不要自动改派 |
| 下载目标已存在 | 使用新的目录；客户端拒绝覆盖 |
| 503 / 507 | 分别检查过载/不确定 I/O 与明确配额；按原幂等身份重试 |

服务名记录在私有 `session.json` 的 `prefix` 中。用 `journalctl --user -u <prefix>-worker-a.service` 读取对应日志，或查看 `worker-a/tasks/` 下的原生记录。不要发布整个 session 文件中的 token。完成后运行[清理命令](index.md#cleanup)。
