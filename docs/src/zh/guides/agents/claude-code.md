# Claude Code

```bash
pvisor run --safe --stage ../stage-001 -- claude
```

`--safe` 按可执行文件名匹配 Agent，自动放行对应 API 的 HTTPS 目标，并为 HOME 状态提供独立视图。凭据通过显式 `--pass-env NAME` 授予，或由已配置的 Gateway 持有上游 Key。

!!! note "TODO"
    补注入方式（代理／base URL）、HOME 与状态生命周期、已知限制与受支持版本。

