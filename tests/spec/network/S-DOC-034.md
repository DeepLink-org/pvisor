# S-DOC-034：G01 启用默认 proxy


建议场景：适合配置出站网络、代理访问或禁止网络的任务。

**语义**：记录为 explicit-proxy，拦截强度为 cooperative。它只约束经过代理的流量，不能据此认为直接 socket 已被禁止。

**理由**：只给出 `--overlaynet`，省略值时启用默认 proxy 模式。

**违反示例**：默认 proxy 请求没有记录 explicit-proxy/cooperative，或 capture 产物缺失。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-034 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --overlaynet -- "$CASE_TRUE"
> CASE_COMMAND
>
> bundle_expect network.interception.driver explicit-proxy
> bundle_expect network.interception.strength cooperative
> bundle_contains artifacts capture
> CASE
PASS S-DOC-034
```
