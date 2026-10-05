# 以 Job 为核心的检查点与分叉命令设计

> CLI 更新：独立 `pvisor snapshot` 已删除。以下旧接口/测量属于记录中的历史制品，不是当前可执行指南；当前入口与能力范围见[CLI 参考](../reference/cli.md)。


> 状态：分阶段实现中。2026-10-03。工作区检查点与命令管理已接入；普通 Job 的完整 execution 保存/恢复仍未接通。
> 用户已接受该设计，并追加约束：`run` 尽可能不变。独立 `pvisor snapshot` 已删除；本文保留完整执行能力接入 Job 生命周期的目标接口。完整快照、持久压缩去重和私有副本分叉的已有实现作为底座，不因此宣称普通 Job 已可保存。

## 1. 对象与基本约束

**Job 是唯一运行对象。** Job 保持任务身份、策略、工作区目标、运行历史和父子关系。`stage` 是用户管理该 Job 的持久目录，其中的文件暂存和执行检查点具有不同的生命周期。

| 对象 | 身份与用途 | 可变性 |
| --- | --- | --- |
| Job | JobId；被 run/status/kill/suspend/resume/fork 操作 | 生命周期、当前 Attempt 和工作视图可变 |
| Attempt | AttemptId；一次 runner 生命周期 | 新 runner 必须取得新 AttemptId 和执行权代际 |
| workspace | 工作视图、staged changes、preimages、接受记录 | Job 运行或 apply/drop 时产生新代 |
| checkpoint | CheckpointId；保存一致的文件或完整执行状态 | manifest 与内容不可变；可有增量父引用 |
| 内容对象 | 按内容身份保存的文件或 RAM 块 | 不可变；Job、checkpoint、恢复引用负责 pin |

检查点类型：

- `workspace`：文件版本、暂存变更、前像与对应配置；从它启动新命令，不续接 CPU/RAM。
- `execution`：CPU、RAM、设备、文件版本、工作视图归属及兼容性；从它继续保存的进程。

压缩、去重、全量/增量属于 checkpoint 存储机制，不产生另一套 Job。

## 2. 命令树

```text
pvisor
├── run                    创建并运行 Job
├── status                 查看 Job 状态、能力和保留对象
├── review                 查看绑定文件版本的变更
├── inspect                在只读 Job / checkpoint 文件视图中检查
├── apply                  接受停止 Job 的选定文件变更
├── drop                   丢弃停止 Job 的待接受文件变更
├── kill                   终止 Job；不创建检查点
├── suspend                保存执行检查点，退出 runner，保留 Job
├── resume                 同一 Job 从当前挂起检查点继续
├── fork                   从文件或执行状态创建新 Job
├── checkpoint
│   ├── create             为 Job 创建一致检查点
│   ├── list               列出 Job 内的检查点
│   ├── show               查看类型、来源、依赖、兼容性和引用
│   ├── delete             删除没有保留引用的检查点
│   └── gc                 回收所在内容库的无引用对象与遗留暂存
└── extensions             查看伴随工具
```

`tui/replay/cache/memory-pool` 保持其工具定位，不产生与 Job 并行的产品实例身份。默认执行短写 `pvisor -- COMMAND` 继续等价于 `pvisor run -- COMMAND`。

不新增 `job run`、`vm run` 或 `snapshot run` 作为目标公共工作流。

## 3. 选择 Job 与存储

统一 `JOB` 参数允许：JobId、stage 目录、现有 run.json / Job 内部路径，以及 `last`。沿用现有 resolver；多个候选时拒绝，不能静默选择。

- `last` 只在当前工作区和指定存储索引中解析一次；不自动寻找“最近可挂起”或“最近停止”的 Job。
- 名称 `--name` 是展示信息，不用名字代替稳定身份；多个同名 Job 不构成选择规则。
- `status/review/inspect` 可省略 JOB，等价于 `last`。
- `apply/drop/kill/suspend/resume/fork` 必须显式给 JOB；写 `last` 视为显式选择。
- `--output-dir/-o` 继续控制现有 Job 查找根；显式 stage 路径不依赖另一个索引。它不代表把运行内容导出到新目录。
- `CHECKPOINT_ID` 在指定 Job 中解析；允许唯一摘要前缀，歧义或归属不符时拒绝。本版不引入 `latest` 检查点别名，历史选择必须明确。
- `resume` 只使用 Job 持久记录的挂起 head，不接受 `latest` 或任意旧检查点；从旧状态运行必须 fork。

逻辑布局如下；既有上层文件布局可以兼容映射，不要求一次迁移：

```text
stage/job-a/
├── job.json
├── attempts/<attempt-id>/
├── workspace/             # 可写状态、preimages、接受记录
└── checkpoints/
    ├── index.json
    └── <checkpoint-id>/   # manifest、CPU/设备状态、内容引用

<per-user-content-store>/  # 不属于另一套 VM 实例管理器
└── content/<content-id>
```

checkpoint manifest、归属和引用位于 stage 内，共享内容可以位于公共内容库。配置指定内容库；常用命令不要求另传 `--store`。迁移 stage 跨文件系统需显式导出/导入工具，不能承诺直接移动目录后硬链接引用仍可用。

`drop` 不删除 Job 记录、Attempt 历史或 checkpoint。checkpoint 删除不修改当前 workspace。Job 删除及归档不在本版 CLI 中新增，禁止用 `drop` 暗含这些动作。

## 4. 命令语法与行为

### 4.1 run：唯一启动入口

```sh
pvisor run [现有运行、策略、记录与资源选项] \
  [--stage PATH] [--name NAME] -- COMMAND...
```

保留现有 `--vm/--executor`、`--rootfs`、`--safe`、`--mount`、网络和 Gateway 选项及配置优先级。

**不新增必需的启动标志，不要求 `--checkpointable`。** 保存能力由已经选择的 executor、rootfs、文件系统、设备和网络配置推导，在 Job 元数据与 status 中记录。用户仍通过原有方式启动 Job，在需要时请求 suspend、checkpoint 或 execution fork。

run 的参数解析、默认 executor、命令与环境、CPU/内存、stdio、TUI、策略安装、stage 选择与结果接受语义尽可能保持。它不因“以后可能保存”而静默关闭 DAX、网络或设备，不把 rootfs 改成另一种视图，也不默认开启压缩/pager或完整复制所有输入。

首版支持矩阵仍限定经过验收的配置；完整快照、Run/Attempt/Bundle、stage、文件版本与前像归属接通后才声明对应能力。历史独立 snapshot runner 不构成普通 Job 已接通的依据。

保存请求分两层检查：启动时记录静态能力；操作时核对动态条件（活跃连接、文件句柄、设备在途访问、兼容性、空间和执行权）。不满足时返回具体原因，保留原 Job 继续执行；不能重启工作负载来补装能力，也不能静默降低隔离政策。

如果实现需要可选配置，应先复用现有 executor 配置，并单独讨论必要性；不要为了接入快照增加另一套 run 参数组。`--ram-storage raw|compressed` 放在实际捕获状态的命令上，不放在 run 上。


### 4.2 status 与 review

```sh
pvisor status [JOB] [--json]
pvisor status --review [JOB] [--diff]       # 保留兼容入口
pvisor review [JOB] [--checkpoint ID] [--diff] [--json]
```

status 至少显示 JobId、AttemptId、executor、工作区/stage、执行状态、workspace generation、待接受状态、suspended head、checkpoint 能力与阻止原因、来源 Job/checkpoint。

生命周期状态与文件状态分列：例如 `suspended`＋`pending changes`，不能以“PID 不存在”显示为完成。

review 输出必须绑定 JobId、workspace version / checkpoint ID、change-set 摘要、目标和前像。停止/挂起 Job 从稳定视图读取；运行中 Job 如能获得一致文件版本，则冻结捕获 workspace checkpoint 后继续运行，再生成 diff；不能取得一致视图时明确拒绝。`--checkpoint` 查看历史文件版本，不授予直接把它 apply 到当前 Job 的能力。

现有 diff 大小限制继续保留。后续变化会让旧 review 不再对应当前 workspace；apply 必须重新核对版本、选定集合和宿主前像。

### 4.3 inspect

```sh
pvisor inspect [JOB] [--checkpoint ID] -- COMMAND...
```

只读检查当前稳定文件视图或指定 checkpoint。它不是 guest 中的 exec，也不执行 guest CPU 状态。对运行中可变文件视图使用与 review 相同的一致性门槛。

### 4.4 apply 与 drop

```sh
pvisor apply JOB [--path RELATIVE_PATH ...] \
  [--include GLOB ...] [--exclude GLOB ...] [--all] [--target PATH]
pvisor drop JOB
```

选择性 apply、原态/冲突检查、目标选择和原有接受记录合同继续保留。不自动 apply，不因 fork/suspend 改变审批或接受身份。

两命令只允许已确认停止的 Job。运行、过渡、挂起和执行权 unknown 均拒绝。挂起 Job 必须先 `kill JOB` 放弃在原 Job 内继续执行的资格，才可以处理其工作视图。

成功后工作视图生成新代；保留的不可变检查点不被改写。对旧 checkpoint 的执行恢复只能创建新 Job，并建立其独立工作视图，不回滚现有 Job 的接受历史。

分支继承分叉点的前像。两个分支先后 apply 同一目标时，后者必须检查宿主是否已变化，不因同源而免除冲突检查。

### 4.5 suspend

```sh
pvisor suspend JOB [--ram-storage raw|compressed] \
  [--timeout DURATION] [--request-id ID] [--json]
```

仅操作 running Job。冻结 CPU 和设备写入，在同一冻结边界保存 execution checkpoint 及文件版本；发布后确认源 runner 退出，提交 suspended head，输出 JobId、CheckpointId 和状态。

`--ram-storage` 控制本次 RAM 的持久编码，默认 raw，compressed 为现有分块压缩去重。它不改变在线 pager，也不是增量保证。尚无 `--incremental` / `--lazy` 空选项。

`--timeout` 是等待和冻结的预算；超时不是强杀或证明 worker 已结束。操作仍在收尾时返回持久 operation ID，拒绝并发恢复。不得因客户端退出而在“已发布但源可能运行”状态启动另一 runner。

重复挂起已 suspended 的 Job返回现有 head，不生成新检查点。workspace-only 或无有效 execution capture 的 Job明确返回能力错误。

### 4.6 resume

```sh
pvisor resume JOB [--tui] [--request-id ID]
```

仅从 suspended head 继续同一 Job。保持 JobId，生成新 AttemptId 和执行权代际；验证兼容性与完整引用，创建可写工作视图，安装状态后执行。

不接受新命令、rootfs、CPU/内存或策略覆盖。配置变化通过 fork workspace 或新 run 完成；首版不提供不可靠的执行态参数变更。

命令前台持有终端，与 run 类似；`--tui` 仅改变宿主展示，不能假装恢复旧宿主 TTY/管道或外部连接。允许的 console endpoint 重建须由 executor profile 声明。

运行失败：开始执行之前失败时保留 suspended head，可重试；开始执行之后失败不能自动把 head 标为仍可无副作用重试。记录新 Attempt 及实际状态，恢复到旧点需要显式 fork。

### 4.7 fork

```sh
pvisor fork JOB [--state workspace|execution] [--checkpoint ID] \
  [--stage NEW_PATH] [--name NAME] [--request-id ID] \
  [--ram-storage raw|compressed] [--tui] [-- COMMAND...]
```

默认 `--state workspace`，保留现有文件 fork 行为。选择 execution 必须显式给出；不能根据“找到了 VM 检查点”自动切换。

| 条件 | workspace fork | execution fork |
| --- | --- | --- |
| running，无指定 checkpoint | 获得一致文件保存点；无冻结能力则拒绝 | 冻结、发布完整保存点、建立子 Job 和引用，再解冻父 Job |
| suspended，无指定 checkpoint | 从挂起点的文件版本派生并启动命令 | 引用 suspended head 并创建子 Job，不改变父 Job |
| stopped，无指定 checkpoint | 从稳定文件视图捕获并派生 | 拒绝；无活 CPU 状态，不自动选择历史点 |
| 指定 checkpoint | 必须 workspace 类型或显式提取 execution 的文件视图；首版只接受类型一致 | 必须 execution 类型，来源与能力匹配 |
| 替换命令 | 允许；缺省沿用保存点的命令配置 | 拒绝；继续保存的进程 |

指定不可变 checkpoint 时不冻结正在运行的父 Job，也不读取其当前可变文件；只校验归属、配置与保留引用。若要取父 Job 当前状态，省略 `--checkpoint`。

`--ram-storage` 只用于新捕获的 execution 保存点；workspace 模式或复用已有 checkpoint 时拒绝该选项。`--stage NEW_PATH` 必须为空或不存在；不覆盖旧 Job。省略时沿用现有持久 Job 存储选择规则，生成新目录。

子 Job 使用新 JobId、新 AttemptId、独立执行权和独立可写 workspace/RAM；manifest记录父 JobId、源 checkpoint ID 与状态类型。guest PID/boot ID可保留保存时的值，不能把它们当作全局 Job 身份。未来网络与管理身份须有独立重绑定合同。

复制或 COW 克隆工作状态，继承不可变内容引用；不递归复制父 Job 的执行锁、socket、临时写入、终态、接受授权或 checkpoint 索引。父 Job 创建 checkpoint 的历史记录属于父；子 Job 持有来源引用，不冒充拥有父的历史。

父冻结仅覆盖新保存点持久提交与子分支引用/身份建立。子 runner 启动可在父解冻后发生；子启动失败不应保持父无限暂停。失败子 Job 记录可查询并正常回收；整个事务通过 request ID避免重试产生多个子 Job。

### 4.8 checkpoint 管理

```sh
pvisor checkpoint create JOB [--kind workspace|execution] \
  [--ram-storage raw|compressed] [--timeout DURATION] \
  [--request-id ID] [--json]
pvisor checkpoint list JOB [--kind workspace|execution] [--json]
pvisor checkpoint show JOB CHECKPOINT_ID [--json]
pvisor checkpoint delete JOB CHECKPOINT_ID [--json]
pvisor checkpoint gc JOB [--json]
```

create 默认 workspace。运行中 execution create 冻结、发布独立对象后解冻父 Job，**不挂起 Job**；其捕获事务与 execution fork 共用。stopped Job不能创建 execution checkpoint。suspended Job可引用既有 execution head而不启动 guest；若返回既有对象，输出 `reused=true`。

create 不提供覆盖已有 checkpoint 的选项。内容可全量、压缩、去重或后续增量；CLI始终输出不可变身份和实际格式。

show 显示类型、来源 Job/Attempt、文件版本、兼容性、增量父依赖、逻辑/编码字节以及阻止删除的引用。逻辑/编码字节不是物理驻留或密度指标。

删除受到 suspended head、子 Job来源、增量后代、活跃恢复和暂存写入的引用保护。首版遇到这些引用直接拒绝，不提供 `--force` 或隐式级联。后续 detach/压实也必须先建独立对象，再原子替换引用。

gc 通过 JOB定位内容库；可以回收该库中任何无引用内容及遗留暂存，不只扫描一个 Job。JSON输出明确 scope及分类计数。活跃写入、恢复、保留检查点和分支依赖不得被回收。

### 4.9 kill

```sh
pvisor kill JOB [--json]
```

running：沿用现有正常终止请求，未确认退出前保持 stopping/unknown，不能提前允许 apply/drop。

suspended：无 runner可杀，明确终止该 Job的继续执行资格，释放 suspended head 的自动 pin，转为 stopped，终止原因记为 cancelled，不伪造 guest 自然退出码；检查点作为历史保留对象及其他分支引用不被删除。原 Job不能再 resume；仍可显式 fork retained execution checkpoint。

stopped：幂等成功。挂起/恢复/分叉的过渡状态拒绝冲突操作；执行权 unknown不把缺失 PID视为已停止。

## 5. 状态、执行权与故障

```text
running → suspending → suspended → resuming → running
running → stopping → stopped
suspended → stopped                  # kill，放弃原 Job 的继续执行资格
```

starting、failed和unknown沿用/扩展实际记录，不把所有失败压成 stopped。文件接受状态独立记录，例如 pending、partially_applied、applied、dropped。

每个 Job同一时刻最多一个有效执行权。控制操作持久记录 request ID、来源代际、发布 checkpoint、源退出确认与目标 Attempt；恢复和 fork 在取得执行权之前不能进入 guest。

关键失败边界：

1. 发布前失败：不能开放目标恢复；可安全解冻时父继续，否则记录真实 pending/unknown。
2. 发布后、源退出未确认：对象可保留，但原 Job resume拒绝，不能仅凭对象存在认为 suspend完成。
3. 子身份已建立、runner未启动：父可继续；重试同 request ID定位同一个子 Job。
4. 响应丢失：查询 status及操作记录，不再次创建副作用。生命周期命令可用 `--request-id`重试；参数摘要不一致则拒绝。
5. 删除/GC失败：保留能追溯的暂存与引用，重试收尾；不能为了释放空间先丢恢复依赖。

进程内锁、磁盘锁和控制面租约职责分开。本版仍同宿主；跨节点执行权不由复制 stage获得。

## 6. 输出与错误

短控制/查看命令支持 `--json`，stdout输出单个结果对象，诊断到stderr。结构包括 schema_version、operation、request_id、job_id、attempt_id、state、checkpoint_id以及适用字段；status还包括文件状态和capability。

run/resume/fork为前台执行命令，保留 guest stdout/stderr，Job/stage信息写入宿主诊断通道；本版不提供会与工作负载输出混杂的 `--json`。自动化读取明确 stage的status JSON，run现有result-file合同继续保留。

CLI语法错误退出2，管理操作失败退出1；前台工作负载退出码按原约定传播。成功挂起使前台runner返回管理成功，但Job记录为suspended，不记录“工作负载自然完成”。

错误需提供稳定分类及可执行的说明：JOB_BUSY、CAPABILITY_UNSUPPORTED、CHECKPOINT_KIND_MISMATCH、CHECKPOINT_REFERENCED、COMPATIBILITY_MISMATCH、TARGET_CONFLICT、EXECUTION_UNKNOWN、STORAGE_FAILURE。不得静默降级成文件fork、重启命令或忽略请求的隔离策略。

## 7. 使用流程

以下命令是目标接口示例，不表示当前产品可运行。

### 工作区结果接受：保持原工作流

```sh
pvisor run --safe --stage ./stage/task -- codex
pvisor review ./stage/task --diff
pvisor fork ./stage/task --stage ./stage/alternative -- codex
pvisor apply ./stage/task --path src
# 或：pvisor drop ./stage/task
```

### 完整暂停与继续

```sh
# 终端 A；沿用正常 Job启动路径
pvisor run --vm --stage ./stage/task \
  --rootfs /path/to/rootfs -- bash

# 终端 B；保存并释放源 runner
pvisor suspend ./stage/task --ram-storage compressed
pvisor status ./stage/task

# 新 runner、相同 Job；前台继续
pvisor resume ./stage/task
```

### 运行中的整机分叉

```sh
# 另一终端；父 Job 短暂冻结，然后继续
pvisor fork ./stage/task --state execution \
  --stage ./stage/branch-a --ram-storage compressed

# 从同一个已保存分叉点产生另一分支，无需再次冻结父 Job
pvisor checkpoint list ./stage/task --kind execution
pvisor fork ./stage/task --state execution --checkpoint CHECKPOINT_ID \
  --stage ./stage/branch-b
```

### 挂起后接受结果

```sh
pvisor review ./stage/task --diff
# 先放弃在原 Job 内继续；不删除历史 checkpoint
pvisor kill ./stage/task
pvisor apply ./stage/task --all
```

## 8. 兼容与落地顺序

1. 保留现有run、stage选择、workspace fork与apply/drop合同，先扩展Job生命周期和checkpoint类型。
2. 将独立快照runner的捕获/恢复能力纳入既有VM executor、Run/Attempt/Bundle、stage、控制通道及执行权；复用原run入口，不通过改派到旧snapshot run来伪装统一。接通文件版本与前像归属，再声明对应保存能力，不增加必需的run标志。
3. 交付suspend/resume、status真实投影及失败收尾；完整VM保存不以正常退出伪装。
4. 交付execution create/fork的capture-and-continue事务，共用捕获实现；stage私有复制为基线，增量和COW在后端逐步优化。
5. 交付checkpoint引用、删除/GC及历史选择。改变内部布局时保留旧Job读取能力，不能直接把旧逻辑checkpoint解释成execution。
6. `status --review`保留为review兼容入口。`fork --checkpoint`保留，但要求checkpoint类型匹配显式state。
7. 独立 snapshot 命令已删除，不保留隐藏 legacy 前端。旧对象仍由存储 SDK 验证兼容性、来源与引用，不伪造普通 Job 记录；迁移能力需明确验收。

迁移阶段不提供看似统一、实际绕过stage的run别名。每条新命令开放前须有Job生命周期、文件接受和真实VM验收；未支持的平台/配置返回能力错误。


## 9. run 行为保留的验收门槛

接入实现必须比较改动前后的正常运行行为，不能只验收新命令：

- 原有 run 参数、默认执行短写、配置优先级及命令参数/环境保持。
- 相同配置得到相同的 executor、策略请求、挂载视图与 stage 位置。
- 未请求保存时不产生 execution checkpoint，不额外压缩或全量复制 RAM/文件树。
- 正常完成、取消和失败的输出、退出码、Bundle与apply/drop合同保持。
- 不能保存的 Job仍可以正常运行；suspend或execution fork给出能力错误，不改变该Job状态或权限。
- 原有workspace fork默认行为保持；运行中execution fork的改变必须由显式操作触发。

Job与Attempt记录可以增加版本化、兼容读取的能力和生命周期字段；这些字段不改变用户的run语义。

## 10. 当前实现与验收边界 {#10-当前实现与验收边界}

本轮保持 `run` 的解析、默认配置、rootfs、DAX、网络和执行器选择。新增工作区功能复用既有 RunRecord、Job lease、逻辑检查点、OverlayFS 前像和 Run Bundle；没有通过旧 snapshot runner 创建伪 Job。

| 功能 | 当前实现 |
| --- | --- |
| `review [JOB] [--checkpoint ID]` | 内置；取得 Job lease 后读取稳定 upper；刷新文件变更，保留历史 Bundle 执行证据；JSON 标明两者的边界 |
| `status --review` | 保留兼容入口 |
| `checkpoint create JOB` | 停止且 stage 为 staged 时保存 upper、前像、策略、AttemptId 和 workspace generation；默认 workspace |
| workspace create `--request-id` | 持久 receipt；重试返回原对象；原对象已删除则拒绝重新捕获，不重用 key 创建新结果 |
| `checkpoint list/show/delete` | Job 归属校验、唯一前缀解析、歧义拒绝；损坏的已发布对象报错；删除检查持久分支引用 |
| `checkpoint gc JOB` | **本阶段 scope 为 job_workspace_transactions**：仅回收本 Job 的 `.pending-*`、`.deleted-*`；尚未接入共享 execution 内容库，不声称全库 GC |
| `fork --state workspace` | 保留默认命令重启语义；增加 `--stage`、`--name`；独立复制 upper 和前像，不复制控制 socket、执行锁和接受记录；分支引用保留在 `source-checkpoint.json` |
| `inspect --checkpoint ID` | 挂载指定工作区检查点的只读文件视图 |
| `apply/drop` | 显式 Job；取得 lease 后仍核对停止终态和完成时间，拒绝失去进程但缺少终态记录的 Job；成功后推进 workspace generation |
| `kill` | 已确认停止时幂等；`--json` 区分已停止和已发送终止请求 |
| suspend/resume/execution create/execution fork | 接入能力拒绝边界；普通 Job 当前均返回 `CAPABILITY_UNSUPPORTED`，不冻结、不复制、不改状态；**不是完整执行功能已交付** |

工作区检查点继续沿用已有模型：固定 staged upper 与冲突前像，`lower_dirs` 仍是外部路径引用。因此它不等价于独立完整文件树；宿主 lower 的外部变化不受 Job lease 保护。review 的 `file_view` 明确标明这个边界，历史 diff 仍相对于外部 lower；apply 继续重新检查前像。后续接通 execution 保存时需要完整封存所有相关层，不能把该模型直接提升为整机保存点。

本阶段文件分支 pin 使用 manifest 硬链接。子 stage 需要与父 checkpoint 在同一文件系统；跨文件系统会明确拒绝。pin 不由 drop/kill 释放；Job 删除/归档接口尚未提供，因此保留分支的 checkpoint 不能通过 delete 强制删除。

当前工作区的检查点管理、review、inspect 和 fork 会保守地取得源 Job lease。因此指定历史检查点时，源 Job 正在运行也会被拒绝。独立的 checkpoint 元数据锁及运行中历史读取还未交付。运行中 workspace 捕获需要真实冻结边界，不能从正在变化的 upper 复制来冒充一致版本。

完整执行态仍需继续完成：

1. 普通 VM 的 Overlay/DAX 文件层封存与设备状态捕获，并支持保存后文件 inode/句柄重绑定；不能静默改变原有 run 配置。
2. 通过现有控制通道和 supervisor 实现 capture-and-continue、源 runner 退出确认、suspended head 与新 Attempt 的执行权交接。
3. execution checkpoint 的 Job 归属、共享内容引用、删除/全库 GC、恢复失败持久状态及真实 Job VM 验收。

旧 `snapshot` 命令已删除，其完整副本存储对象仍由底层 SDK 使用，没有被转换成 Job checkpoint。普通 Job 的完整执行恢复仍取决于 execution profile 的能力与验收。

本轮验证：`just fmt` 通过；核心与 TUI 在启用 Gateway 下的严格 Clippy 通过；`just test pvisor` 最终 319 项通过、4 项跳过。新增测试包括真实文件分叉、前像复制、分支引用与 drop 保留、重复请求、损坏 manifest 和能力拒绝无状态改变。测试期间发现并修正既有 vsock 用例对后台 worker 调度的错误假设，改为检查队列完成和实际 RST 内容；内存诊断用例曾因页状态变动返回 WouldBlock，随后回归通过。没有以这些测试宣称普通 Job 完整 VM 保存/恢复已验收。
