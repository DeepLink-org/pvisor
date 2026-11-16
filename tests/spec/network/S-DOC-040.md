# S-DOC-040：G07 审查被代理拒绝的具体目标


准备：安装 curl。

**语义**：curl 请求失败；`status --review` 的 Network access observations 可指出 `blocked.example:80` 被拒绝。host proxy 是协作式边界，本例只证明经过代理的请求被拦截。

**理由**：通过 pVisor 注入的代理访问一个明确拒绝的域名，确认 Session 记录的是具体目标和拒绝次数，而不只是网络失败总数。请求在策略层被拒绝，不依赖该域名真实可访问。

**违反示例**：请求失败，但审查记录没有 blocked.example:80 的具体拒绝目标和次数。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-040 --timeout 180 <<'CASE'
> require_python3
> require_curl
> case_setup
> case_run nonzero <<'CASE_COMMAND'
> pvisor --overlaynet-deny blocked.example -- /bin/sh -c \
>   'curl --fail --silent --show-error --noproxy "" -x "$http_proxy" --max-time 2 http://blocked.example/'
> CASE_COMMAND
>
> bundle_contains network.intercepted.targets 'HTTP blocked.example:80'
> bundle_contains network.intercepted.targets '"denied": 1'
> CASE
PASS S-DOC-040
```
