# 命令与场景参考

把一次任务接入脚本时，分别处理启动参数、执行结果和文件接受决定。普通 host 运行直接写入工作区；需要评审后再接受文件时，使用 `--safe`、`--ask` 或显式 `--stage`。

## 从运行到接受文件

```bash
pvisor run --safe --overlaynet-deny-all --stage ../stage-reference-001 -- /bin/sh -c 'printf proposal > report.txt'
pvisor status --review --json ../stage-reference-001 > ../review-reference-001.json
pvisor inspect ../stage-reference-001 -- /bin/cat report.txt
pvisor apply ../stage-reference-001 --path report.txt
```

运行结束后，`report.txt` 留在暂存区；`inspect` 读取提案，`apply` 才接受所选文件。不接受时用 `drop` 丢弃剩余暂存改动。运行与 apply 的退出码分别检查，不能因为任务返回 0 就自动接受文件；冲突和失败处理见[退出码与错误](exit-codes.md)。参数语法与存储默认值由 [CLI 参考](cli.md)定义。

## 读取记录与使用协议

`status --json` 提供状态概况；`status --review --json` 提供完整 Run Bundle。自动化先识别格式版本和必需字段，再检查执行状态、实际控制与净文件改动；缺失观察不能当成零或通过。字段与查询分别见 [Run Bundle](run-bundle.md)和[机器可读输出](json-output.md)。

共享镜像缓存的 handle、认证和失败行为遵循[缓存协议](shared-image-cache.md)，与 Job 的暂存和 apply 生命周期分开。完整机器状态的保存与恢复还需要对应能力；workspace checkpoint 只保存文件提案，不继续旧进程内存，使用前按 [CLI 的 execution checkpoint 契约](cli.md#full-vm-execution-checkpoints)检查支持条件。

## 核对可执行行为

[场景附录](cases.md)保留产品命令、语义声明与断言，也是 semspec 的 DOC 规格源。代码块中的 runner 夹具不能直接当作普通 shell 命令运行；仓库入口 `just cases` 执行对应检查。检查成功与人工批准分别记录，不因 PASS 改写人工审核账本。
