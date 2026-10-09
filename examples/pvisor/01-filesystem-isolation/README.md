# 1.1 pVisor 的文件系统隔离

**问题：Agent 修改文件时，原始项目目录会不会立即改变？可复现结论：base 保持原值，upper 和 Bundle 都记录 2 个变化，`filesystem_non_bypassable=false`。**

`run.sh` 把 `base/` 作为 OverlayFS base，让 Agent 修改一个文件并新增一个文件，然后
展示 base、stage upper 和 Run Bundle。`just examples 01-filesystem-isolation` 对这些产物执行回归断言。该实验测量
事务工作区隔离，不声称 Host 进程无法访问其他宿主路径。

## Run

```bash
./run.sh
just examples 01-filesystem-isolation  # 从仓库根执行并验证
```

## Links

- [pVisor examples](../README.md)
- [Review and apply effects](../../../docs/src/zh/guides/review-apply.md)
