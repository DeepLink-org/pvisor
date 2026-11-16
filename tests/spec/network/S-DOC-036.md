# S-DOC-036：G03 allow、deny 和带宽限制组合


建议场景：适合配置出站网络、代理访问或禁止网络的任务。

**语义**：记录为 allowlist 模式，允许 `api.example.com:443`，拒绝 `10.0.0.0/8`，并把 `1mbps` 记录为每秒 125000 字节。本例不实际发请求。

**理由**：组合配置允许目标、拒绝网段和针对目标的带宽上限。只有通过代理的流量才受这些规则约束。

**违反示例**：allow、deny 或带宽规则中的主机、端口、速率被丢失或改写。

```scrut {output_stream: combined, timeout: 190s}
$ python3 "$TESTDIR/../fixture.py" doc S-DOC-036 --timeout 180 <<'CASE'
> require_python3
> case_setup
> case_run success <<'CASE_COMMAND'
> pvisor --overlaynet-allow api.example.com:443 \
>   --overlaynet-deny 10.0.0.0/8 \
>   --overlaynet-limit api.example.com=1mbps \
>   -- "$CASE_TRUE"
> CASE_COMMAND
>
> bundle_expect network.policy.mode allowlist
> bundle_expect network.policy.rules.0.host api.example.com
> bundle_expect network.policy.rules.0.ports.0 443
> bundle_expect network.policy.deny_rules.0.host 10.0.0.0/8
> bundle_expect network.policy.limits.0.host api.example.com
> bundle_expect network.policy.limits.0.bytes_per_second 125000
> CASE
PASS S-DOC-036
```
