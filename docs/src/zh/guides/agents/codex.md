# Codex CLI

```bash
pvisor run --safe --stage ../stage-001 -- codex
pvisor status --review ../stage-001
```

Codex 的 HOME 状态只有在 `--safe` 下才会独立暂存；直接运行 Codex 时保留宿主环境继承，以维持账号与路由配置。

!!! note "TODO"
    补配置方式、HOME／CODEX_HOME 生命周期、已知限制与受支持版本。

