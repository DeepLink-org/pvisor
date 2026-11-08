---
description: 通过 Homebrew 或 pip 安装 pVisor，接入 Pi、DSH、Codex 插件，沿用原生命令交接成果。
---

# Pi、DSH 和 Codex 插件

在 Agent 中调用 pVisor 暂存任务，完成后使用原生 `spawn → status/review → apply/drop` 流程交接成果。通过 Homebrew 或 pip 安装 runtime，npm 仅分发 Agent 适配器。

!!! note "从 0.1.0 升级"
    使用 0.1.1 或更新版本的插件接入 Homebrew/pip runtime。已发布的 0.1.0 仍依赖旧 npm runtime 包，重装不能解决该依赖。远程 marketplace tag 需要单独发布；仅发布 npm 包不会让这些 ref 自动可用。

下述输出收集、离线 Rust、fanout 和批量清理功能始于插件 0.1.2，后续测试版改进了 drop 和 apply 展示。0.1.6 增加原生 Host workspace fork、受控产物导出和结构化 review。0.1.8 修复 DSH 监控面板的发现问题，0.1.11 修复后台刷新痕迹。当前 0.1.12 测试版以运行记录和逐文件内联 diff 展示任务。Registry 安装命令需要先完成发布，源码测试通过不代表已经发布。

## 安装 {#install}

准备 Node.js 22.19+、目标 Agent，并通过 [Homebrew 或 pip](../../start/installation.md) 单独安装 pVisor。Host 暂存支持 Linux x64、macOS arm64，需要满足 [Host 暂存前提](../../start/installation.md#local-prerequisites)。插件不会安装 runtime、自行提权安装 FUSE、启用扩展或改变 namespace 策略。

启动 Agent 宿主时，确保其 PATH 中可以找到 `pvisor`。桌面应用可能不继承终端 PATH，必要时在宿主环境中设置 `PVISOR_PLUGIN_BINARY` 为已安装 CLI 的绝对路径。插件忽略空或相对 PATH 项，并在工作前检查必需的 CLI 参数。不兼容时拒绝执行并提示升级；缺少选择性 apply 的旧版（例如 0.3.1）不满足要求。插件与 runtime 独立版本管理。

选择你实际使用的宿主：

```sh
# Pi
pi install npm:pvisor-agent-plugins@0.1.12

# DSH: install only the profile you use
dsh plugin --profile web add pvisor-agent-plugins@0.1.12
dsh plugin --profile desktop add pvisor-agent-plugins@0.1.12

# Codex CLI: run from your project
codex plugin marketplace add DeepLink-org/pvisor --ref plugins-v0.1.12
codex plugin add pvisor@pvisor
codex mcp add pvisor -- npx --yes --package pvisor-agent-plugins@0.1.12 pvisor-agent-mcp --project "$PWD"
```

DSH Desktop 也可以在 **Plugins → Add plugin** 中输入 `pvisor-agent-plugins`。只安装你实际使用的 profile。

Codex marketplace 安装工作流 skill，原生 `codex mcp add` 用当前项目绑定工具；`npx` 仅安装固定版本的插件，无需全局安装，runtime 必须预先安装。没有插件管理能力时可仅用 MCP。捆绑 MCP 的 cwd 可能是插件缓存目录，不能用它推断用户项目。

安装后从目标项目启动新会话。调用 `pvisor_doctor`，检查二进制路径、项目路径和 runtime 版本；它检查必需的 CLI 能力，但不证明 FUSE 挂载或安全隔离可用。Pi 使用调用上下文的 cwd，DSH 使用发起 Agent 的 cwd。MCP 缺少显式项目时拒绝启动，不回退到插件目录。

需要实际检查沙箱能否启动时，用 `{"probe":true}` 调用 `pvisor_doctor`。它运行一个小型安全任务，并在 `probe` 中返回 selector，像其他任务一样审查或显式 drop。不会自动清理候选，也不把启动成功扩展成隔离认证。

## 执行任务 {#spawn}

要求 Agent 调用 `pvisor_spawn`，例如传入：

```json
{
  "argv": [
    "sh",
    "-c",
    "printf 'candidate\\n' > accepted.txt; printf 'temporary\\n' > discarded.txt"
  ],
  "timeout_seconds": 120
}
```

该工具调用原生 `pvisor spawn`，使用操作员配置的运行环境、`--safe`、拒绝全部出站网络、捕获 stdio 和执行期限。源码版默认优先 VM，不会未经授权降级到 Host；运行示例前先完成下方环境配置。不加载个人 Agent 预设。期限为 1–600 秒，默认 120 秒。每次 spawn 创建独立 Session，相关编辑/测试放在同一次调用中，或显式 fork 已停止且仍保留的候选；全新的兄弟 spawn 看不到未接收成果。

保存返回的绝对 stage selector。插件工具 `pvisor_status`、`pvisor_review`、`pvisor_apply_preview` 与原生 CLI 使用同一个 selector，无需转换插件 ID。不要使用 `last`。失败、取消或未知完成状态仍可能留下候选文件，不自动重试或清理。

### 读取捕获输出 {#output}

Spawn 将任务的 `stdout`/`stderr` 与 `cli` 中的启动器诊断分开返回。`exit_code` 保持 CLI 退出码，`task_exit_code` 和 `task_state` 描述已完成的任务。每个任务输出流上限 256 KiB，`truncated` 同时保留原生捕获截断。适配器通过原生验证过的 `status --review --json` 收集输出，预算 8 MiB，不直接读取私有完成记录。

`output_status: unavailable` 表示没有经过验证的终态输出，或读取超过预算。检查 `output_error` 和 `cli`，不能把缺少输出当成成功。适配器不会自动查询被中断的 owner；核对任务后，用相同 selector 调用 `pvisor_output`，不要重新执行任务。

## 准备离线 Rust 任务 {#rust-offline}

操作员可以只读暴露已安装的 Rust 工具链，同时保持拒绝全部网络和私有构建状态。启动 Agent 宿主前，选择真实的工具链目录：

```sh
export PVISOR_PLUGIN_RUST_TOOLCHAIN="$(dirname "$(dirname "$(rustup which rustc)")")"
```

Doctor 返回 `environment.kind: rust-offline` 和所选工具链。根目录必须包含真实的 `bin/cargo`、`bin/rustc` 和 `bin/rustdoc`，并位于项目、插件状态和私有临时路径之外。rustup shim 目录或整个 `~/.cargo` 不是工具链根目录。操作员选择该目录等于显式授权读取，不应选择包含密钥的目录。

在任务执行之外的可信终端中准备依赖，前提是项目已提交 Cargo.lock：

```sh
cargo vendor --locked --versioned-dirs vendor > vendor-source-config.toml
```

将生成的 source 配置节合并到 `.cargo/config.toml`，不要覆盖已有设置。项目内保留 `vendor/`、锁文件和 source 配置。准备 vendor 时可能需要联网；插件不会拉取依赖或运行 rustup 安装。

要求 `pvisor_spawn` 执行 argv `["cargo","test","--offline","--locked"]`。该配置提供 rustc/rustdoc 的直接路径，设置 `CARGO_NET_OFFLINE=true`，将 `CARGO_HOME` 和 `CARGO_TARGET_DIR` 放在每个任务的私有 HOME 中。不继承用户的 Cargo registry、凭据、配置、Rust wrapper 或 rustup 覆盖项。系统 linker、C 头文件、原生库和 build script 的依赖仍需预先准备。小型 vendored fixture 成功不代表所有 Rust 项目都能构建。

## 收集实验并清理 {#experiments}

向 `pvisor_fanout` 传入具名实验计划：

```json
{
  "concurrency": 2,
  "tasks": [
    {"name": "a", "argv": ["sh", "-c", "echo candidate-a > result.txt; echo a"]},
    {"name": "b", "argv": ["sh", "-c", "echo candidate-b > result.txt; echo b"]}
  ]
}
```

每一项是独立的原生 Session。限制为 1–8 个任务、1–4 个并发任务，每项仍有 1–600 秒期限。报告保留名称、精确 selector、有界输出、失败和耗时。`elapsed_order` 按耗时列出已完成任务，不代表正确性排名或受控性能测量。选择前先审查和测试候选结果。未知完成状态不重试，取消后未启动项标为 `not_started`。

操作员也可以将相同 JSON 保存为 `experiments.json`，调用 CLI driver：

```sh
npx --yes --package pvisor-agent-plugins@0.1.12 pvisor-agent-fanout \
  --project "$PWD" --input experiments.json
```

嵌套的宿主终端无法安全启动原生 CLI 时，改用插件工具。Driver 只执行和收集，不接受或丢弃候选。

`pvisor_tasks` 列出当前项目的插件任务、原生生命周期/overlay 状态及 selector。接受 `limit`（1–50，默认 20）和上一页返回的 `next_cursor`。分页是当前观察，不是快照；格式损坏或其他项目的插件记录不列出，新插入任务可能需要重新扫描。

DSH 的 `pvisor_drop_batch` 接受 `{"selectors":["/exact/stage-a","/exact/stage-b"]}`，Pi 的 `/pvisor drop-batch` 使用同样的 JSON。Drop 不再额外询问确认：任何修改前先检查全部 1–50 个显式 selector，执行时再由原生 drop 逐项准入。逐项结果为 `dropped`、`already_applied`、`refused`、`unknown` 或 `not_attempted`，不回滚先前的 drop。对于已结束、全部接收的 Session，只有原生 status 和干净的空 dry-run 确认没有剩余候选或 pending apply 时，才返回成功的 `already_applied` 空操作。这不代表回滚或删除，原生拒绝码保留在 `native_exit_code`。部分 apply、未知/运行中/挂起状态、待恢复操作或无效证据都不会被当成正常空操作。Codex MCP 不提供批量修改工具。

没有空闲 TTL 或递归目录删除来清掉待处理成果。原生 drop 处理候选，插件任务记录仍保留；失败或未知结果可能保留独立的私有 `scratch_home`，不属于项目成果。这不是完整的磁盘空间回收器。

## 分叉保留的候选 {#fork}

父任务已停止并暂存了 `branch.txt` 时，调用 `pvisor_fork`：

```json
{
  "selector": "PARENT_SELECTOR",
  "argv": ["sh", "-eu", "-c", "cat branch.txt; printf 'child\\n' > branch.txt"]
}
```

使用 spawn 返回的精确 selector，占位符不是查询别名。原生 CLI 创建 workspace checkpoint，将文件与冲突 preimage 恢复到独立的子 stage，再执行显式命令。父候选仍可审阅，apply 前原项目不变。响应包含 `parent_selector`、新的子 selector、捕获输出及原生 `lineage`；后续 status/review/apply/drop 使用子 selector。

来源必须是当前项目/插件所属、完成证据有效且暂存数据仍保留的终态任务。公共证据还须确认 Host 文件/网络边界已强制执行，且能力明确为拒绝全部网络。失败或取消的候选也可能有用。运行中、未知、挂起、全部 apply 或已 drop 的来源会被拒绝。原生 workspace 策略重建还会拒绝不支持的 VM/container/Gateway 或 prepared-workspace 来源，不会用更弱的默认策略启动全新任务代替。

安全策略、只读授权和期限由原生继承。Fork 不暴露 timeout、executor、share 或 checkpoint 覆盖项。子任务使用新的私有 HOME，配置 Rust 时也有新的离线状态；操作员配置变化不会给继承的来源策略增加授权，工具链/profile 改变后应启动新父任务。传输 watchdog 为 630 秒，原生父期限仍然生效。

这是文件候选分支，不是进程内存、Agent 对话或全部宿主 lower 的不可变副本。父子 stage 位于插件状态的同一文件系统。不自动重试结果不确定的 fork，失败后仍可能保留子记录/selector。

## 导出产物而不接受 {#export}

操作员须先配置一个已存在的私有产物根目录，位于项目、插件状态和已配置工具链之外：

```sh
mkdir -m 700 "$HOME/pvisor-artifacts"
export PVISOR_PLUGIN_EXPORT_DIR="$HOME/pvisor-artifacts"
```

不要覆盖已有根目录或盲目修改其权限，复用前核对所有者和 `0700` 模式。让 Agent 宿主带此环境重启，Doctor 会报告规范化根目录；未设置时 export 禁用。

保留的子任务包含 `branch.txt` 时，调用 `pvisor_export`：

```json
{
  "selector": "CHILD_SELECTOR",
  "paths": ["branch.txt"],
  "output": "child-artifact"
}
```

插件使用显式项目相对路径和 `--output ROOT/child-artifact` 调用原生命令。`output` 只能是 1–128 个字母/数字/`._-` 组成、以字母或数字开头的新目录名；不允许绝对路径、分隔符、`..` 穿越或隐式 all 选择。各项目共享该根目录的名称空间，应使用唯一名称。原生检查拒绝与项目/stage/storage/authority 重叠的目的地，已有文件、目录或链接绝不替换。

仅支持 Linux directory upper。原生完成验证和锁允许仍保留的 completed/failed/cancelled 任务；active、unknown、applied 和 dropped 来源被拒绝。符号链接、特殊文件、类型替换和 opaque 依赖不受支持。出错后不会改用直接复制或放宽策略。

常规文件内容位于 `files/`，原生 `manifest.json` 记录逻辑元数据以及仅供参考的目录/删除项。Export 不执行删除，不重建所有元数据，不包含未选中的 lower 文件/依赖，也不创建完整 patch 或 merged snapshot。来源仍可按生命周期允许的条件 review、fork、export、apply 或 drop；应在全部 apply/drop 清掉暂存数据前导出证据。

`export_status: published` 只表示产物目录已发布。`manifest` 是原生响应，包含来源结果、条目、删除项及 `partial_results`；来源任务失败不代表证据提取失败。`destination` 和 `manifest_path` 标识产物位置。不增加审批，不接受变更、不自动清理来源，也不上传。

Export 的传输 watchdog 为 120 秒。取消进程或丢失/无效/截断的响应可能与发布竞争；`export_status: unconfirmed` 会保留精确目的地。先检查再决定下一步，不自动重试、覆盖或删除不确定的产物。

## 一个 Agent 绑定一个实例 {#agent-binding}

保存任务 selector 不会重定向 Agent 的内置 Shell、读取或补丁工具。当前任务插件尚未提供持久的逐 Agent 绑定。

需要一个持续交互的暂存 Agent 进程时，将整个 Agent 启动在原生 Session 内，让其工具在内部工作，参照[整个 Codex 托管启动](codex.md#run-codex)。显式设置认证和网络策略，不在这个 Session 内嵌套任务插件。

需要在同一个长驻 VM 中反复执行外部命令时，Linux 的[容器暂存绑定](../../reference/cli.md#container-staged-binds)支持 `run`/`exec`，停止容器后再 review/apply/drop。它有独立于原生 Session 延续的生命周期。让 DSH 的某个子 Agent 绑定此类实例，还需要宿主适配器路由其文件和 Shell 工具；当前插件不能靠保存实例 ID 来强制实现。

## 审查并接收 {#handoff}

### 在 DSH Web 中监控任务

插件 0.1.12 在右侧栏导航页提供只读 **pVisor** 页面，
也可通过标签栏的新增入口打开。安装后重启 DSH Web 并刷新页面。
浏览器扩展以 DSH **0.2.2-rc.1** 为基线，请先检查 `dsh --version`。
旧 alpha 版的工具测试不代表 UI 兼容，也不宣称已验证 Desktop/Electron。

从 0.1.7 升级：旧包通过 `/dsh` 包子路径挂载，DSH 0.2.2-rc.1
扫描浏览器模块时会跳过这种名称，所以工具可用而面板缺失。
0.1.8 改为挂载包根入口，保留 `/dsh` 别名。更新后停止并重启 DSH Web，
再刷新浏览器。

面板显示当前 Session 项目的任务分页，分别展示执行状态和 stage 状态、
结构化候选条目以及按需读取的终态输出。本页包含 live/运行中/未知任务时，
每次读取完成一秒后顺序采样；否则间隔两秒。后台读取不切换加载提示，
不清空行；数据未变（包括仅时间戳变化）时不向 UI 提交更新。
选中任务被新任务挤出当前页时仍保持打开。这是状态采样，不是推送，
也不保证观察到极短任务的运行阶段。隐藏标签、后台 Session 或隐藏浏览器页面时，
暂停采样并取消请求。
读取失败会明确标记保留的观测已经过时。每页最多十个任务，
支持上一页/下一页；本页计数不是项目总数，也不是冻结快照。
变更分别统计文件和目录；最多显示 200 项并明确提示截断，
完整有界清单通过 `pvisor_review` 获取。捕获输出是有界的历史终态记录，
不是实时日志流。首版界面使用中文标签，不提供 run/apply/drop/fork/export
控制或自动清理。面板经 DSH 的 owner 认证 API 通道复用现有 Backend，
不另开无认证服务，也不让浏览器直接读取文件系统。

面板使用 DSH 公共 Button/Pill/Tag 控件和主题变量。任务在分页前按
创建时间从新到旧排序，显示原生命令摘录和时间。新记录保存适配器创建时间；
旧记录以记录文件出生时间（不可用时取修改时间）估计，标注 `≈`。
`pvisor_tasks` 默认仍按 ID 排序；传 `order: "newest"` 可启用创建时间排序，
后续 cursor 请求应保持相同 order。

列表行优先使用已有显示名，否则从命令推导标题；命令降为等宽第二行，
不添加 argv 序列化引号。只展示一个主状态，并配相对时间和耗时。
这些显示推导不能用于重新执行命令。精确时间收进折叠详情，
采用中文 24 小时格式和浏览器时区。

**待处理** 默认隐藏本页已经清理/接收的历史，**全部** 可查看它们。
筛选仅针对当前页，较早的保留候选可能在其他页。点击项目名可展开项目、
排序和历史说明。空页提供 **查看全部** 操作；分页只显示可用的方向。
**已清理** 表示原生 drop 已清空候选 upper/work 数据，
任务元数据和执行证据仍保留，并不表示候选文件还在。这不是磁盘占用测量，
也不是自动垃圾回收。点击任务进入 **文件变更** 和 **终态输出**。
选中文件后在列表旁显示内联 diff（窄面板上下排列），不再单设任务级 Diff 标签。
变更类型使用中文，`+N −M` 来自经过验证的原生 hunk；目录不虚构大小，
二进制、跳过或截断输入的行数保持未知（`null`）。含控制字符或非 UTF-8 的文件名
保留清单，但不猜测文本边界。最多渲染 200 个文件条目和 2000 行 diff，并提示限制。
Diff 是单独读取的有界观测，已清理候选不能从空 stage 恢复。

尚未实现终端附着。已经结束的任务不是持久 shell；交互式进入候选需要
新建隔离的 workspace 分支和受管理的 PTY，不能以宿主 shell 或直接访问
stage 目录代替。

`pvisor_review` 返回 `review_status`、`changes[]`、`counts`、`in_scope`、
`outside_scope`、`unclassified`，以及原生 `handoff` 和 `review_context`。
条目保留原生 `path`、存在时的 `path_bytes`、`kind`、`old_type`、
`new_type`、`size_bytes` 和 `mode`；路径相对于 `filesystem.target`
（插件项目）。`counts.total` 是条目数，不是文件数：`file`、
`directory`、`symlink`、`other` 和 `unknown` 分开计数。删除使用
`old_type`，其他条目使用 `new_type`。原生不支持的项目条目仍保留在
`handoff.unsupported_project_changes`；review 不授予 apply/export 权限。

使用 `{"selector":"STAGE_FROM_RESULT","include_diff":false}` 只取结构化清单。
默认仍在 `stdout` 中保留有界文本 diff。
`diff_status: "separate_observation"` 表示 JSON 与文本分别读取，
不是同一个冻结快照；`truncated` 或 `unavailable` 不能视为完整 diff。
当前清单在 apply/drop 后刷新，执行证据仍是历史记录。原生 review
未知、格式无效、截断或失败时，`review_status` 为 `unavailable`，
`changes`/`counts` 为 `null`，而不是成功的空清单。原生无法分类范围时，
`in_scope` 和 `outside_scope` 为 `null`，不是零。Review 不运行测试，
也不替代重新执行的冲突检查 apply preview。

使用已经通过 Homebrew 或 pip 安装的原生 CLI：

```sh
pvisor --version
```

将返回的 selector 保存为终端变量 `stage` 后：

```sh
pvisor status "$stage" --json
pvisor review "$stage" --diff
pvisor apply "$stage" --path accepted.txt --dry-run --diff
pvisor apply "$stage" --path accepted.txt
pvisor drop "$stage"
```

检查已接收文件并重新运行相关测试。仅在剩余候选不再需要时 drop；全部接收后跳过。原生所有权、完成凭据和冲突检查保持有效，插件不会绕过运行 owner 的 stage 锁。Drop 不撤销已 apply 文件或外部副作用，详见[成果审查](../review-apply.md)。

Pi 也提供命令；apply 要求交互确认，drop 不再询问：

```text
/pvisor status STAGE_FROM_RESULT
/pvisor review STAGE_FROM_RESULT
/pvisor apply {"selector":"STAGE_FROM_RESULT","paths":["accepted.txt"]}
/pvisor drop STAGE_FROM_RESULT
```

DSH 的 `pvisor_apply` 在展示原生预览后要求 Harness 的 `Allow once`，拒绝、取消或审批不可用时不写回。单次和批量 drop 不再请求审批，Pi 命令在没有交互 UI 时也可执行 drop；它们直接丢弃显式选择的候选，但仍保留项目归属、selector 和原生生命周期检查。不引入自动 drop。Codex MCP 不提供发布或丢弃工具，由用户在终端执行上面的原生命令。不改变宿主的其他权限设置，skill 不能限制已有宿主 Shell 权限的 Agent。

插件预览前先调用原生 status 核对并恢复 Session 控制状态，再调用 `apply --dry-run`；不会直接创建内部锁文件，也不会接受项目文件。

### 理解 apply 摘要 {#apply-summary}

审批卡使用简短段落，即使宿主折叠换行也可阅读：Agent 变更目的（标为未验证）、项目、原生变更数量、最多三个路径、剩余数量和任务标识。不放原始 diff、原生 banner 或完整状态目录 selector。较大的变更集合会明确提示从完整预览查看省略的路径。

独立的预览结果仍保留项目/stage、请求路径、按类型列出的实际选中变更、必需依赖、剩余数量及所选范围的文本详情。未选中的 inventory 不会被显示为已获准的变更集合。被阻塞的预览列出候选 inventory 和 blocker，明确说明本次请求未写入项目文件，不发起审批。

Runtime 0.3.2 不允许 `--json` 与 `--diff` 同用，适配器因此分别读取原生 JSON 和所选范围的文本 dry run，收集文本后再核对计划。计划变化、响应损坏、来源/项目不匹配，以及详情收集失败或截断，都不会打开审批框。这只检查收集期间的展示一致性，不是事务，也不在用户决定期间保留写入权。执行时仍进行原生冲突检查。

机器可读 JSON 保留在 `stdout`，人工展示使用 `summary`，另有独立的 `diff`、`preview_state` 和 `approval_ready` 字段。大量变更和节选会明确标注显示截断；缺少文本 diff 不代表内容未变化，预览也不会运行测试。

DSH 的 `pvisor_apply` 要求提供非空 `summary` 字符串（1–512 字节），先审查 diff，再用用户的语言简要解释所选变更做了什么、为什么做；不要粘贴日志或 diff。Pi 的操作员命令可选填写。审批卡将它标为未验证的 Agent 目的，不当作原生事实或测试证据；该文字不能改变选择或授权执行。没有匹配变更时返回信息性的空操作，不再请求审批。

## 配置与边界 {#scope}

| 环境变量 | 默认值 | 含义 |
| --- | --- | --- |
| `PVISOR_PLUGIN_BINARY` | 宿主 PATH 中的 `pvisor` | 已安装 CLI 的绝对路径；覆盖项无效时不回退 |
| `PVISOR_PLUGIN_STATE_DIR` | `~/.local/state/pvisor/agent-plugins/v1` | 项目外的私有任务记录和 stage |
| `PVISOR_PLUGIN_PROJECT` | 未设置 | 手动启动 MCP 时的显式项目；建议用原生 `codex mcp add ... --project DIR`；Pi、DSH 使用调用上下文 |
| `PVISOR_PLUGIN_EXECUTION` | `auto` | 优先 VM；`vm` 仅 VM，`host` 显式保留安全 Host 执行 |
| `PVISOR_PLUGIN_ALLOW_HOST_FALLBACK` | `false` | 仅字面值 `true` 授权 `auto` 在任务启动前降级 |
| `PVISOR_PLUGIN_VM_ROOTFS` | 未设置 | 显式准备的 Linux rootfs 与工具；可信所有者、其他用户不可写，与项目/state 分离，禁止宿主 `/` |
| `PVISOR_PLUGIN_RUST_TOOLCHAIN` | 未设置 | 操作员选择的真实工具链根目录；只读访问，使用私有离线 Cargo 状态 |
| `PVISOR_PLUGIN_EXPORT_DIR` | 未设置（export 禁用） | 项目/state/工具链之外已有、当前用户所有的 `0700` 产物根目录；模型只选择新子目录名 |

状态目录要求当前用户所有、权限 0700。任务不能读取其他项目的插件记录。任务记录不保存 argv 或凭据，runtime 输出和记录仍按原有规则保留，避免输出密钥。

每个任务使用新的私有 HOME 和 XDG 状态路径，不继承宿主 HOME 的登录状态、用户配置或缓存。持久化 stage 仍在配置的状态目录中；失败或中断后可能保留临时 HOME，它不属于待接收的项目成果。依赖用户 HOME 配置的工具需要单独准备，不应通过关闭隔离绕过。

### VM 优先策略 {#execution-policy}

源码实现要求 runtime 提供 `vm-check --json`，不表示已发布的 0.3.2
二进制具备此接口。启动 Agent 宿主前先配置准备好的 Linux rootfs：

```sh
pvisor vm-check --json
env PVISOR_PLUGIN_VM_ROOTFS=/absolute/prepared-linux-rootfs dsh web
```

Linux 预检使用 Agent 宿主的权限打开 KVM、检查 API 版本并创建空 VM。
Apple Silicon 路径尝试创建和销毁 HVF VM，尚未在这台 Linux 主机上验证。
Firmware 发现是独立结果：`bundled_unloaded` 不代表动态库已加载或 guest 已启动。
缺少虚拟化能力、firmware 或 rootfs 时，默认阻止执行。预检畸形、失败或中断
需要修复或升级，不允许降级。插件不修改设备权限、不下载镜像，也不默认将宿主
`/` 当作 rootfs；guest 工具和依赖需预置在 rootfs 内。

如允许降级，在 Agent 宿主启动配置中保存操作员授权，工具参数不能改变它：

```sh
env PVISOR_PLUGIN_ALLOW_HOST_FALLBACK=true dsh web
```

仅 `auto` 在收到有效的否定预检后使用此授权。
`PVISOR_PLUGIN_EXECUTION=vm` 始终拒绝降级；
`PVISOR_PLUGIN_EXECUTION=host` 显式保留原安全 Host 工作流，不要求新的 VM 预检命令。
Pi、Codex/MCP 使用相同环境变量。VM 启动失败不会转到 Host 重试。
Doctor 区分预检与可选的真实暂存 probe；任务行显示选定后端和已授权降级，
不将后端选择当作失败任务已经启动 guest 的证明。
Host workspace fork 要求显式 Host 策略或允许 `auto` 降级。
现有 Host 工具尚未提供 VM workspace fork、VM Session 终端或 Container 策略。
Rust 只读共享 profile 也仅用于 Host；VM 工具链需放在准备好的 rootfs 内。

这些是任务级适配器，不覆盖宿主的内置文件、Shell、补丁、后台任务或其他 MCP 工具。任意网络/executor 覆盖、任务 stdin、整个 Agent 的工具路由与工具级保存点不是模型输入。需要整个 Codex 进入 stagebox 时使用已有 [Codex 托管启动](codex.md#run-codex)，不要嵌套任务插件。

## 验证与维护 {#verify}

在临时项目中调用 `pvisor_doctor`，再运行前面的双文件任务。确认宿主未提前出现文件，review 后只 apply `accepted.txt`，drop 另一项，已接收文件仍应保留。Pi、DSH 同时检查批准与拒绝两条路径。启动失败时保留安全设置，按报错修复系统前提。

宿主 fixture 不等于真实 Pi UI、DSH Web/Desktop、Electron 或 Codex 插件加载验收。实际接入时记录 Node、pVisor、Agent 版本、工具目录、项目路径和审批行为；不据此宣称云模型端到端或免审批验证通过。

升级前审查待处理任务。通过宿主包管理器更新插件，通过 Homebrew 或 pip 单独更新 runtime，再重启并运行 Doctor 检查兼容性。不要在任务运行时替换 runtime。Codex 固定的 marketplace ref 与 npx 版本需一起更新。通过宿主管理器卸载 `npm:pvisor-agent-plugins`、`pvisor-agent-plugins` 或 `pvisor@pvisor`；独立 MCP 配置用 `codex mcp remove pvisor` 删除。卸载插件保留 runtime 和候选文件，不递归清空状态目录。
