# S-DOC-007：A07 默认写入直接到 workspace


**语义**：命令退出后，`direct.txt` 直接出现在原 workspace，记录中没有 OverlayFS stage。

**理由**：验证普通 host Session 的默认可写 lower；无需为日常命令额外选择执行器或 stage。

**违反示例**：没有请求 stage 的 host 写入没有直接出现在工作区。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-007 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor -- /bin/sh -c 'printf direct > direct.txt'
> CASE_COMMAND
>
> test "$(cat direct.txt)" = direct
> record_expect overlay null
> CASE
PASS S-DOC-007
```
