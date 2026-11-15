# S-DOC-037：G04 deny-all 不可通过环境变量绕过


建议场景：适合配置出站网络、代理访问或禁止网络的任务。

准备：安装 curl。

**语义**：命令失败，结果记录 no-network 和不可绕过边界。外网自身不可用也会使 curl 失败，因此本例不能单独证明隔离有效。

**理由**：验证禁止网络后，清除常见代理变量仍不能访问外网。Linux 使用 network namespace，macOS 使用 Seatbelt；需要宿主安装 curl，命令故意发起网络请求。

**违反示例**：清除代理环境变量后 curl 成功连到外网，或安全产物未声明强制边界。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-037 --timeout 180 <<'CASE'
> require_python3
> require_curl
> case_setup
> case_run nonzero <<'CASE_COMMAND'
> pvisor --overlaynet-deny-all -- /bin/sh -c \
>   'unset HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy; curl --max-time 2 https://example.com'
> CASE_COMMAND
>
> bundle_expect network.policy.mode no-network
> bundle_expect safety.network_non_bypassable true
> bundle_expect run.state failed
> CASE
PASS S-DOC-037
```
