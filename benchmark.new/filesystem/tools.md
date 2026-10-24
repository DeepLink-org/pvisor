# Native filesystem：metadata 与 read

Benchmark: B-FS-TOOLS ([注册表](../REGISTRY.md#b-fs-tools)), role user-facing.

## Motivation

观察固定热缓存输入上，原生 metadata/read 完整 Python 命令的耗时。
本实验不代表 VM、七工具内部阶段或跨后端任务性能，不推导配对改善比例。

## Experiment design

两行共享 32 个目录、2,048 个小文件和一个 64 MiB 数据文件。准备器流式生成输入，
完整校验目录、大小和内容，并为选中行各执行一次不计入统计的预检。
每行顺序执行 3 次预热、30 个正式样本，保留全部有效慢值；单次超时为 30 秒。

`fs_timing.py` 用公共 runtime 的 `perf_counter_ns`，从创建子进程前计到正常退出并收集输出后。
解释器启动、管道和负载自身校验计入时间；输入准备、观察者启动、行前后完整 fixture 校验及审计不计时。
每次要求对应操作的精确成功 JSON。semspec 每行调用脚本一次，接收完整统计行，不再套一层计时。

```sh
just benchmark filesystem/tools
just benchmark filesystem/tools --row native-read
just benchmark filesystem/tools --output benchmark.new/filesystem/.data/native-new
```

以下路径占位符由准备器展开；正式执行入口先准备、冻结并预检，再执行和审计。
不要直接对未展开的源文件执行 `filter --execute` 或 `bench`。

<!-- semspec: table id=native-filesystem -->
```json
{
  "native-metadata": "$ python3 @REPO@/benchmark.new/filesystem/fs_timing.py @BATCH@ metadata",
  "native-read": "$ python3 @REPO@/benchmark.new/filesystem/fs_timing.py @BATCH@ read"
}
```

## Data and analysis

每行保留 1 次预检、3 次预热和 30 次正式记录、原始纳秒样本及样本标准差。
审计复算时钟、统计、逐次 argv、退出和输出，核对输入与冻结 harness 后才更新 `tools.render.json`。
失败和超时阻止发布；`--row` 仅更新选中行，`--output` 保留成功或失败的批次证据。

默认临时批次在成功后清理。新 Python 时钟数据与历史 hyperfine 结果分开，
不覆盖历史用户页；详细证据结构见 [README](README.md#代码与证据)。
