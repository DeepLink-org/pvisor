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
"$SEMSPEC_BIN" render benchmark.new/process/.data/results/smoke.semspec.md
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
再追加 `.data/results/<entry>.semspec.csv`，在模板旁生成
`<entry>.render.json` 和引用它的结果 Markdown。
CSV 保留完整精度及批次来源；展示 JSON 将毫秒格式化为两位小数。重复发布同批次不重复追加；
不同 CSV schema 拒绝混合。可以通过 `--csv` / `--markdown` 指定新目标。
Markdown 单独一行写 `{{ table("../../tools.render.json") }}`；文档构建只读取已生成 JSON，
不会运行实验。CSV 和结果 Markdown 均写入入口目录下的 `.data/results/`；模板旁的
`.render.json` 是被 Git 忽略的本地预览，文档发布仍使用独立的公开资产目录。
历史 results 报告的表格直接保留在 Markdown 中，CSV/JSON 保留原字节归档，不混入新批次。

历史派生表位于各主题的 `.data/derived-tables-20261018/`，重复预览表位于
`.data/generated-tables-20261018/`。此次迁移的完整路径、字节数、SHA-256 和修改前的
报告保存在本目录 `.data/generated-tables-prune-20261018/`；配置、实验入口、
测试夹具、文档构建用表格和必要公开下载继续跟踪。

`--prepare` / `--conclude` 重置并验证每次输入输出；失败不发布。构建、rootfs、镜像和服务
预先准备，不能将旧多样本 campaign 当成单次被测命令。hyperfine 按条件顺序独立采样，
不宣称随机配对、因果改善比例或置信区间。Ready、内部阶段、密度和整体 cgroup 内存等
不同指标不能用完整命令时间替换。工程和诊断实验保持 registry 中各自的角色。

## 迁移范围与验证

注册入口已核对行命令规则；尚无单次负载的模板保持未准备。
全部活动 ID 已改为 JSON 入口。启动、process、filesystem/tools、replay 有具体命令；其他条目的
`requires-single-condition-command` 模板显式使用 null，准备时拒绝测量。它们还需从旧
`benchmark/pvisor/` 拆出单次负载及正确性 hooks，不能用历史统计或任意命令填充。

`just benchmark [smoke|nightly]` 在构建后走 prepare → semspec bench → audit → render；CI 使用同一
新 harness 测量候选和基线，并从原始导出执行现有 15% 回归门禁。
验证：`just test-semspec` 和 `just test-py discover -s benchmark.new/tests`。

## 文档表格

[docs.json](docs.json) 登记中英文 benchmark 用户页及相关分析页的 benchmark ID、角色、
必要公开 CSV，以及构建所需动态表格的 JSON 引用和发布绑定。固定历史表格直接写在 Markdown 中，
保留原来的数字、统计口径、链接和对齐；构建时不再依赖逐张历史 JSON。
公开 CSV 由 [public_assets.py](public_assets.py) 白名单控制，只保留必要用户统计；
`csv_path(repo, filename, locale)` 定位共享下载，中英文共用一份。
正文已有的静态表、逐轮逐环境记录、历史和工程中间表不默认另行导出。
完整 CSV/JSON 留在原批次的本地 publication 目录或 `.data/` 归档，动态 JSON 只保留构建所需投影。
本地化展示 JSON 留在文章旁。
复测发布的结果与自动生成的覆盖范围表继续通过 `semspec render` 读取 JSON。
重复表头可使用 `headers`，链接、代码和对齐通过显式渲染选项保留。

```sh
python3 benchmark.new/docs.py check
python3 benchmark.new/docs.py publish "$batch"
```

`python3 benchmark.new/measurement_coverage.py` 更新 `docs.json` 登记的中英文 benchmark 用户页的复测范围清单。没有匹配负载的页面保留历史数字并明确标识。

`check` 检查文章覆盖、动态 JSON 引用、表结构、未执行命令与 CSV 来源。
公开的新表还必须逐格等于各条件最新 CSV 的展示值，样本数和计时范围不匹配会阻止构建。
`publish` 仅接受 `docs.json` 的 live binding，校验对应时钟的原始样本与正确性 hooks，
仅向共享位置发布白名单内的完整精度 CSV，更新构建所需展示 JSON 与必要用户实验的 provenance，原 MD 无需改写。
必要 provenance 摘要统一保存在 `docs/overrides/assets/benchmarks/`，中英文共用一份；
保留批次、计时口径、运行配置、输入/二进制与证据摘要。完整 manifest、逐文件 harness
清单、宿主路径和软件包收据留在批次 `.data/`。启动页引用 Ready JSON 表；完整
Ready / Exit 指标保留在统计 CSV 中，单行复测保留其他条件的来源配置。
发布器验证 `metric_scope` 与绑定声明的负载配置；中位数与标准差同一行，批次名称放在来源记录，不挤入数值表。
没有 live binding 的条件会拒绝发布；工程 A/B 不写入用户表。filesystem 的历史 hyperfine 结果
保留在本地归档，其用户表、下载和发布绑定已删除；新 Python 时钟数据需单独登记文档表，必要公开 CSV 按白名单发布。改变历史表数据时需同步
登记新来源与摘要，不能让旧 CSV 或旧结论自动冒充新实验。原文章归档留在
`.data/docs-tables-20261011/`；原 CSV 未覆盖。
整理前的完整文档 provenance 归档于 `.data/docs-provenance-20261011/`。
公开图表使用 SVG，不再发布 PDF；工程实验只公开 SVG，完整统计与来源保留在本地 publication。
历史和工程 CSV/JSON、旧 provenance 等保存在 `.data/docs-assets-prune-20261018/`，
迁移清单记录原路径、字节摘要与新位置。裁减公开文件不改变正文表图、统计口径或测量值；
必要公开摘要保留与原始来源的哈希关联。

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

## 公开图表清理

`docs.py check` 拒绝未被文章、展示表或主题引用的公开 SVG。新增图表先在对应文章中引用；
`filesystem/publish_io_curves.py` 只发布当前引用的图，完整图表仍在批次 publication 中归档。
2026-10-18 撤下的无引用 SVG 原字节保存在 `.data/docs-assets-cleanup-20261018/`，
可提交的路径、大小和 SHA-256 索引见 [清理清单](evidence/docs-assets-cleanup-20261018.json)。
清单只覆盖这次公开图表清理，不表示其他历史原始批次已在当前 checkout 核验或完整保留。
