# 文件策略

`--safe` 默认保留暂存工作区，并按预设拒绝 `.ssh`、`.gnupg` 与私钥文件等敏感路径。细粒度控制使用：

```bash
pvisor run --stage ../stage-001 --access 'secrets/**:deny' -- agent-command
```

`--access PATH-GLOB:deny|ask|warn` 在 OverlayFS 视图中拒绝、询问或放行并警告；`--mount …:read` 提供只读共享。

!!! note "TODO"
    补充 glob 语义、授权范围（session／workspace／user）与审批 TUI。
    与 reference/policy 去重。

