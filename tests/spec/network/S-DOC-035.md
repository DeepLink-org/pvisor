# S-DOC-035：G02 自定义 proxy 监听地址


建议场景：适合配置出站网络、代理访问或禁止网络的任务。

**语义**：记录的 OverlayNet 监听地址与请求一致，网络驱动为 explicit-proxy。

**理由**：指定代理监听地址；该参数会自动启用 host proxy。手工运行时确保 18080 端口未被占用；脚本会替换为空闲端口。

**违反示例**：实际监听地址与所请求的 loopback 地址不一致。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-035 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --overlaynet-listen "$CASE_PROXY_LISTEN" -- "$CASE_TRUE"
> CASE_COMMAND
>
> record_expect overlaynet_listen "$CASE_PROXY_LISTEN"
> bundle_expect network.interception.driver explicit-proxy
> CASE
PASS S-DOC-035
```
