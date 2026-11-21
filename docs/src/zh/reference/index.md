# 命令与场景参考

脚本用 `spawn` 启动原生任务，用 `status` 读取执行结果，再决定是否用 `apply` 接收文件。普通 Host 任务直接写入工作区；要在写回前审查改动，启动时使用 `--safe` 或显式 `--stage`。

## 从运行到接受文件

```bash
pvisor spawn --safe --overlaynet-deny-all --stage ../stage-reference-001 -- /bin/sh -c 'printf proposal > report.txt'
pvisor status --review --json ../stage-reference-001 > ../review-reference-001.json
pvisor inspect ../stage-reference-001 -- /bin/cat report.txt
pvisor apply ../stage-reference-001 --path report.txt
```

`spawn` 退出后，`report.txt` 只在暂存区里。`inspect` 读的是暂存副本；直到 `apply` 成功，原项目才会收到文件。不想接收的改动可以用 `drop` 丢弃。脚本要分别检查任务和 `apply` 的退出码：任务成功不代表文件通过审查，写回也可能遇到冲突。见[退出码与错误](exit-codes.md)和 [CLI 参数与存储默认值](cli.md)。

## 读取记录与使用协议

`status --json` 提供状态概况；`status --review --json` 提供完整 Run Bundle。自动化先识别格式版本和必需字段，再检查执行状态、实际控制与净文件改动；缺失观察不能当成零或通过。字段与查询分别见 [Run Bundle](run-bundle.md)和[机器可读输出](json-output.md)。

共享镜像缓存的 handle、认证和失败行为遵循[缓存协议](shared-image-cache.md)，与 Session 的暂存和 apply 生命周期分开。完整机器状态的保存与恢复还需要对应能力；workspace checkpoint 只保存文件提案，不继续旧进程内存，使用前按 [CLI 的 execution checkpoint 契约](cli.md#full-vm-execution-checkpoints)检查支持条件。

## 核对可执行行为

[场景附录](cases.md)保留语义声明与违反示例，并链接 `tests/spec/<feature>/` 中唯一的 Scrut 检查源码。仓库入口 `mise run cases` 执行对应检查。检查成功不等于人工批准，不能因 PASS 改写人工审核账本。

## 场景检查准备

场景附录和 VM 检查共用外部产品 fixture；每条检查使用独立 CASE_ROOT、WS 和显式 SUBJECT_BIN。准备与断言仍需人工审核，摘要审核策略尚待维护者决定。

准备与断言源码：[fixtures/doc.bash](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/fixtures/doc.bash)。
