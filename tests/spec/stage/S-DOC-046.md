# S-DOC-046：J01 host + persistent stage + deny-all + capture + limits


建议场景：适合上线前验证多项能力组合的端到端任务。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：原 workspace 不变；stage 记录 `result.txt`，结果保存 stdout 和资源请求，事件写入指定 JSONL 文件，网络标记为禁止连接。

**理由**：组合使用 host stage、禁止网络、输出采集、JSON 事件和资源限制。命令在隔离视图中写入一个结果文件。

**违反示例**：多能力组合让 result.txt 穿透工作区，或轨迹、资源、网络证据缺失。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-046 --timeout 180 <<'CASE'
> require_python3
> require_stage
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --name host-full \
>   --stage "$CASE_ROOT/host-full" \
>   --overlaynet-deny-all \
>   --stdio capture \
>   --record-destination "$CASE_ROOT/host-full/trajectory/events.jsonl" \
>   --memory 512MiB \
>   --max-processes 64 \
>   --overlayfs-max-size 2GiB \
>   --max-cpu-time 30s \
>   -- /bin/sh -c 'pwd; printf changed > result.txt'
> CASE_COMMAND
>
> bundle_expect run.agent host-full
> test "$(bundle_get run.output.stdout)" = "$(record_get overlay.merged_dir)"
> bundle_expect network.policy.mode no-network
> bundle_expect safety.network_non_bypassable true
> bundle_expect safety.filesystem_changes_staged true
> bundle_contains filesystem.changes result.txt
> bundle_expect resources.requested.memory_bytes 536870912
> bundle_expect resources.requested.processes 64
> bundle_expect resources.requested.cpu_time_ms 30000
> test ! -e result.txt
> test -s "$PVISOR_CASE_ROOT/host-full/trajectory/events.jsonl"
> CASE
PASS S-DOC-046
```
