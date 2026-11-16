# S-DOC-041：H01 Gateway capture 完整组合


建议场景：适合接入 Gateway、模型路由或记录轨迹的任务。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：Gateway 与 stage 成功建立。示例上游是占位地址，`/bin/true` 不发送模型请求；此例不验证对话内容。管理监听地址与运行记录中的 `gateway_listen` 不是同一个服务地址。

**理由**：为需要模型请求记录的 Agent 配置 Gateway。示例设置路由、管理监听端口、完整记录级别、会话头、诊断输出和 Markdown 投影，并保留 stage。

**违反示例**：Gateway/stage 未建立，或 gateway_listen 不是有效的 loopback 地址。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-041 --timeout 180 <<'CASE'
> require_python3
> require_stage
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor \
>   --stage "$CASE_ROOT/gateway-stage" \
>   --gateway-mode capture \
>   --gateway-admin-listen "$CASE_GATEWAY_LISTEN" \
>   --gateway-level full \
>   --gateway-session-header X-Session-ID \
>   --gateway-debug \
>   --gateway-route 'name="default",upstream="https://example.com/v1"' \
>   -- "$CASE_TRUE"
> CASE_COMMAND
>
> record_get gateway_listen | grep -Eq '^127\.0\.0\.1:[0-9]+$'
> bundle_expect network.interception.driver explicit-proxy
> bundle_expect filesystem.state staged
> CASE
PASS S-DOC-041
```
