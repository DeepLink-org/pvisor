# JSON benchmark tables

这里回答的问题：怎样用固定、可审查的 hyperfine 命令生成表格，并保留每次实验的来源？
需求、角色和正确性门禁见 [benchmark/README.md](../benchmark/README.md#registry)。
[registry.csv](registry.csv) 把活动实验映射到按主题组织的 JSON 模板。综合页无独立实验，B-CLUSTER 已退役。

## 模板与执行

每个条件对应一个行命令。hyperfine 行命令固定
样本数、预热、被测命令和 hooks，执行一次实验后直接展开为 N、Median ms、Mean ms、
SD ms、Min ms、Max ms 六列。各列取同一份原始导出，不再重复声明命令：

```json
{
  "Native": "$ hyperfine --shell=none --runs 60 --warmup 3 --command-name native '/bin/true'"
}
```

事件计时使用 Python 行命令，脚本一次返回全部指标。`startup/timing.json` 包含八个
后端，每行同次测量 Ready / Exit；预热不进入统计，正确性失败不发布结果。
旧 hyperfine 启动模板已删除，实际 argv 固定在 `startup/subjects.py`。

```sh
just benchmark startup/timing
just benchmark startup/timing --row Docker
just benchmark startup/timing "pvisor vm"
just benchmark docker_timing
just benchmark smoke
just benchmark filesystem/tools
just benchmark replay --binary target/release/pvisor-replay
```

`just benchmark` 直接输出渲染后的 Markdown 表格，并在模板旁生成同名 `.render.json`：
例如 `docker_timing.json` → `docker_timing.render.json`。默认使用自动清理的临时目录，
只保留最终表格；不追加 CSV、不自动更新用户文档。部分行失败时不覆盖已有结果。
第二个位置参数或 `--row` 可指定启动或文件系统表的一行；行名忽略大小写和多余空格。
复测仅执行该行，更新同名结果文件中的该行并输出合并表格，已有其他行保留，顺序跟随模板。
没有旧结果时只含已测行；整表执行会替换全部结果。拼错行名会在构建和准备前报错并列出可选行。
启动默认选当前可用的前两个 CPU，可用 `--cpu-affinity` 指定；pVisor、镜像与内核需要预先准备。
process smoke/nightly 保留自动构建；其他表使用预构建制品。

需要审计、CI 对比或发布时，显式保留原始证据：

```sh
just benchmark startup/timing --output benchmark.new/startup/.data/measurement
just benchmark smoke --output benchmark.new/process/.data/measurement
```

启动每行保存在 `row-0-<id>/`、`row-1-<id>/` 等子目录中；其他表直接使用指定目录。
`PVISOR_BENCHMARK_OUTPUT` 仍按表名创建子目录，供 CI 保留 `smoke/in.bench.json`。
可对保留批次单独执行 `update.py`；文档发布必须使用完整审计证据。

安装 Python 3.10+ 并构建独立的 semspec CLI；process/replay 另需 hyperfine 2.0+。
准备脚本只冻结输入、展开私有路径和做预检；真正执行实验的入口是 **semspec bench**。
例如 process smoke（2 次预热、10 次样本；nightly 为 10 / 50）：

```sh
cargo build --locked --manifest-path tools/semspec/Cargo.toml --target-dir target/semspec
export SEMSPEC_BIN="$PWD/target/semspec/debug/semspec"
batch=$(python3 benchmark.new/process/prepare.py benchmark.new/process/smoke.json --binary target/release/pvisor)
"$SEMSPEC_BIN" bench --cwd "$batch/workspace" "$batch/in.json"
python3 benchmark.new/update.py "$batch"
"$SEMSPEC_BIN" render benchmark.new/process/results/smoke.semspec.md
```

独立 filesystem 原生负载使用 Python 行命令，见 [filesystem/README.md](filesystem/README.md)。
两行共用一份输入，解释器与负载校验计入完整命令时间，默认无需 hyperfine。手动保留批次：

```sh
batch=$(python3 benchmark.new/filesystem/prepare.py benchmark.new/filesystem/tools.json)
"$SEMSPEC_BIN" bench --cwd "$batch" "$batch/in.json"
python3 benchmark.new/update.py "$batch"
```

启动页八个固定条件见 [startup/README.md](startup/README.md)。`@REPO@`、`@BATCH@`、
`@BINARY@` 等标记由准备脚本展开成实际路径，生成本批次的 `in.json`；原模板保持不变。
`common.py` 共享制品摘要和准备逻辑，`update.py` 校验并发布。process/replay 的完整命令实验由 hyperfine 计时和统计。pVisor VM Ready / Exit 使用独立的 Python `perf_counter_ns` 脚本，
固定参数写在脚本内，返回整行 JSON；它的采样和标准库统计单独标记，不作为 hyperfine 数据。
启动条件的 Python 观察者直接创建被测进程。filesystem 的观察者也直接执行固定 Python 负载，
用 perf_counter_ns 测量完整命令；输入准备、观察者与后续审计不计时。process/replay 继续使用 hyperfine。

`run.py` 统一解析表名、行名和选项；Just 只负责按解析结果构建 process 二进制与 semspec。
准备和发布只接受整行命令，旧的逐单元格命令与 `| json ... * ...` 提取分支已删除。
历史展示 JSON 仍可直接渲染；旧逐单元格批次不再由当前发布器发布，需要按当前模板重新测量。

## 证据与发布

每次准备创建入口目录 `.data/<entry>-<UTC>-<random>/`，拒绝复用已有目录：

- `in.json`：展开路径后的实际命令模板。
- `in.render.json`：保留完整精度的结果表格。
- `in.bench.json`：原始测量导出、命令和表格行/单元格映射。
- `in.bench.logs/`：每个命令的日志；失败也保留。
- `manifest.json`、`harness/`：版本、CPU 亲和性、制品和冻结代码摘要、状态。

`update.py` 检查全部正式样本零退出、条件和提取值与原始导出一致、输入与代码来源未变，
再追加 `results/<entry>.semspec.csv`（启动和文件系统表使用 `.data/results/`），在模板旁生成
`<entry>.render.json` 和引用它的结果 Markdown。
CSV 保留完整精度及批次来源；展示 JSON 将毫秒格式化为两位小数。重复发布同批次不重复追加；
不同 CSV schema 拒绝混合。可以通过 `--csv` / `--markdown` 指定新目标。
Markdown 单独一行写 `{{ table("../tools.render.json") }}`；文档构建只读取已生成 JSON，
不会运行实验。历史 results 文件和包装器计时 CSV 保留原口径，不混入新批次。

`--prepare` / `--conclude` 重置并验证每次输入输出；失败不发布。构建、rootfs、镜像和服务
预先准备，不能将旧多样本 campaign 当成单次被测命令。hyperfine 按条件顺序独立采样，
不宣称随机配对、因果改善比例或置信区间。Ready、内部阶段、密度和整体 cgroup 内存等
不同指标不能用完整命令时间替换。工程和诊断实验保持 registry 中各自的角色。

## 迁移范围与验证

注册入口已核对行命令规则；尚无单次负载的模板保持未准备。
全部活动 ID 已改为 JSON 入口。启动、process、filesystem/tools、replay 有具体命令；其他条目的
`requires-single-condition-command` 模板显式使用 null，准备时拒绝测量。它们还需从旧
`benchmark/pvisor/` 拆出单次负载及正确性 hooks，不能用历史统计或任意命令填充。

B-SWE-RESIDENT-ENG 的 `agent/swe_resident.json` 同样保留为未准备模板。
真实 Agent 运行和 CPU/内存阶段采样仍使用
[`benchmark/pvisor/sweeval/`](../benchmark/pvisor/sweeval/README.md) 的入口，
需要显式配置任务、rootfs 和模型 API。尚未提供本表框架的单条件命令及正确性 hooks，
不能将这些阶段统计作为 hyperfine 的完整命令耗时发布。

`just benchmark [smoke|nightly]` 在构建后走 prepare → semspec bench → audit → render；CI 使用同一
新 harness 测量候选和基线，并从原始导出执行现有 15% 回归门禁。
验证：`just test-semspec` 和 `just test-py discover -s benchmark.new/tests`。

## 文档表格

[docs.json](docs.json) 登记中英文 benchmark 用户页及相关分析页的 benchmark ID、角色、
来源 CSV，以及动态表格的 JSON 引用和发布绑定。固定历史表格直接写在 Markdown 中，
保留原来的数字、统计口径、链接和对齐；构建时不再依赖逐张历史 JSON。
复测发布的结果与自动生成的覆盖范围表继续通过 `semspec render` 读取 JSON。
重复表头可使用 `headers`，链接、代码和对齐通过显式渲染选项保留。

```sh
python3 benchmark.new/docs.py check
python3 benchmark.new/docs.py publish "$batch"
```

`python3 benchmark.new/measurement_coverage.py` 更新 14 个用户页的复测范围清单。没有匹配负载的页面保留历史数字并明确标识。

`check` 检查文章覆盖、动态 JSON 引用、表结构、未执行命令与 CSV 来源。
公开的新表还必须逐格等于各条件最新 CSV 的展示值，样本数和计时范围不匹配会阻止构建。
`publish` 仅接受 `docs.json` 的 live binding，校验对应时钟的原始样本与正确性 hooks，
追加两种语言同字节的完整精度 CSV，更新展示 JSON 与批次 provenance，原 MD 无需改写。
provenance 摘要统一保存在 `docs/overrides/assets/benchmarks/`，中英文共用一份；
保留批次、计时口径、运行配置、输入/二进制与证据摘要。完整 manifest、逐文件 harness
清单、宿主路径和软件包收据留在批次 `.data/`。启动仅发布文档引用的 Ready / Exit
两张 JSON 表，单行复测保留其他条件的来源配置。
发布器验证 `metric_scope` 与绑定声明的负载配置；中位数与标准差同一行，批次名称放在来源记录，不挤入数值表。
没有 live binding 的条件会拒绝发布；工程 A/B 不写入用户表。filesystem 的历史 hyperfine 结果
保留在本地归档，其用户表、下载和发布绑定已删除；新 Python 时钟数据需单独登记文档表与 CSV。改变历史表数据时需同步
登记新来源与摘要，不能让旧 CSV 或旧结论自动冒充新实验。原文章归档留在
`.data/docs-tables-20261011/`；原 CSV 未覆盖。
整理前的完整文档 provenance 归档于 `.data/docs-provenance-20261011/`。

## Replay

B-REPLAY 使用七个固定适配器，每个一条 task 0 合成轨迹，60 次采样、零预热。
它是独立的完整 CLI 时间数据，历史二十条轨迹的分布与一致性结果继续保留。
准备脚本冻结 replay 二进制及轨迹并完成预检；每次 hooks 校验源摘要、精确参数、
完整批次边界、历史观测、零工具执行和真实工作区未变。失败保留证据并阻止发布。
未提供 source-build receipt 时，不能声称冻结二进制来自当前源码。

```sh
export PATH="/path/to/hyperfine/bin:$PATH"
export SEMSPEC_BIN="$PWD/target/semspec/debug/semspec"
batch=$(taskset -c 0,1 python3 benchmark.new/replay/prepare.py benchmark.new/replay/replay.json --binary target/release/pvisor-replay)
taskset -c 0,1 "$SEMSPEC_BIN" bench --cwd "$batch" "$batch/in.json"
python3 benchmark.new/update.py "$batch"
python3 benchmark.new/docs.py publish "$batch"
```

亲和性在准备与 semspec 外层设置，manifest 记录实际选择；被测命令没有 Python 或
`taskset` 包裹。所有适配器按独立条件采样，不声称随机配对改善比例。
