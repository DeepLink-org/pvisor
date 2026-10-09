# semspec 设计：人工审核的语义规格测试

semspec 是独立的 Rust CLI：用 Markdown 陈述项目必须保持的行为，配一段黑盒
Bash 检查。单元测试验证实现意图，语义规格验证产品承诺；PASS 不等于证明或人工批准。
实际工具在 `tools/semspec/`，单包内分模块实现。

## 1. 目标与边界

- 人审核的文字就是执行的规格：性质、理由、违反示例和检查放在同一段落。
- case、断言词汇和引擎均需审核；内容或引擎语义变化后已有审核变为 STALE。
- 每个 case 通过公开 CLI/API，在独立临时工作区中执行。
- 支持已知违反 XFAIL；实际通过时报告 XPASS，要求人重新审查例外。
- 规格是可信代码，runner 不是安全沙箱，不防御拥有仓库写权限的恶意人。

## 2. 核心概念

| 对象 | 含义 |
|---|---|
| Case | 唯一 ID、语义陈述、违反示例、一个 Bash 检查块 |
| Preparation | Markdown 中标记的 Bash 准备块，复用断言与前提判断 |
| Subject | 经 PATH 或运行时参数选择的被测程序 |
| Digest | case、准备文档或引擎的规范化 SHA-256 摘要 |
| Ledger | `REVIEWED.toml`，记录批准摘要、审核人和日期 |
| Verdict | PASS、FAIL、SKIP、XFAIL、XPASS、ERROR |
| Review state | REVIEWED、UNREVIEWED、STALE，与执行结果独立 |

`--require-reviewed` 同时检查 case、所用词汇和引擎，包括 SKIP/XFAIL。

## 3. 人工审核

AI 可以起草规格、修复被测实现、维护 runner 常规测试。AI 不得运行 `approve`、
编辑真实 `REVIEWED.toml` 或遗留 `.approved/` 文件，也不得为获得 PASS 削弱已有
陈述、检查或 xfail。使用方通过 AGENTS.md、CODEOWNERS 和分支保护落实人工审核。
TTY 只阻止非交互误用，不能认证审核者。

批准展示当前全文及依赖，要求输入精确 item，原子更新台账。同一 item 的新批准
替换旧摘要；变更历史由 Git 保留。只维护台账，不再创建批准文本快照，不提供
`diff`、`revoke` 或 `retired`。审查差异使用 Git；已有台账与快照不由迁移自动改写。

## 4. Markdown 文档与代码块

pVisor 的输入是命令行明确选定的用户 case 文档，例如 `docs/src/zh/cases`：先说明使用场景、操作步骤与预期行为，再用代码块给出操作和断言。复制这些 Markdown 即可运行，不需要配置文件、外部 shell 词汇或独立测试脚本。

~~~~markdown
## 第一次运行一个 Job

你已有可信脚本，希望记录执行结果。

**语义**：命令退出 0，文件内容为 hello。

**违反示例**：退出码被吞掉，或文件写到别处。

<!-- semspec: case id=S-USE-001 timeout=60s -->
```bash
journey_setup
pvisor -- /bin/sh -c 'printf hello > hello.txt'
assert_content hello.txt hello
```
~~~~

- **代码块定义 case**：一行 `<!-- semspec: case ... -->` 注释紧接普通 `bash` fence。空白行允许，注释与代码块之间不能插入其他内容。标题只负责叙事，不决定发现、ID 或执行范围。
- `id=S-<DOMAIN>-<NNN>` 必填、唯一；参数按 shell 规则解析，含空格的值加引号。重复、空值和未知参数报错。
- 每个标记绑定一个代码块。同一标题下可有多个 case；未标记的教程示例不执行，代码块里的注释/标题也不被当成 case。
- 摘要绑定 case 所在的完整标题段落（到下一个同级或更高级标题），以及依赖的准备文档。无标题时绑定全文。讲解文字、其他示例与参数变化都使审核失效。
- 普通教程叙事即可承载行为说明。已有的 `**语义**`、`**违反示例**` 和断言保持完整；结构化段落存在时必须非空且不能重复。

| 参数 | 含义 |
|---|---|
| `id` | 项目内唯一的 case ID，域用于选择 |
| `timeout` | 当前 case 的最大执行时间，正数，单位 ms/s/m；覆盖运行时默认值 |
| `xfail-on` | 逗号分隔的 linux、macos，或 all |
| `xfail-reason` | 与 xfail-on 同时出现，说明已知违反与跟踪信息 |

前提条件写在检查中，不采用 requires 或 probe 配置；缺失条件可退出 77。删除的 ID 不复用，历史以 Git 为准。

## 5. 文档中的准备步骤与断言

共用函数、fixture 服务和断言都放在 Markdown 中，代码块前用一行 `<!-- semspec: setup -->` 标记：

~~~~markdown
## 准备步骤

以下函数供同目录 case 共用。

<!-- semspec: setup -->
```bash
assert_content() {
  [ -f "$1" ] && [ ! -L "$1" ] || exit 1
  cmp -s -- "$1" <(printf '%s' "$2")
}
```
~~~~

每个 case 先 source 同目录 `index.md` 的准备块，再 source 本文档的准备块；同一文档按源码顺序执行，index 自身只执行一次。准备块在每个独立工作区中重跑。完整准备文档参与摘要并作为 `@vocab:相对路径.md` 审核对象；执行代码从相同规范化文档字节中抽取。准备叙事发生变化也要求重审。

Markdown 中的 Bash 在 `set -euo pipefail` 下执行。`source` 和 `.` 仍可用于可信规格，但默认学习路线不引用外部脚本；自行引用文件的项目须另行审核这些依赖。runner 不充当安全沙箱。语法检查用 `bash -n`。

| 内置 helper | 行为 |
|---|---|
| `setup FILE.md` | 输出该文档明确标记的准备块，供手动使用与常规回归检查 |
| `tree-state DIR` | 排序记录路径、类型、权限、完整内容 SHA-256、链接目标，不跟随链接，不含时间戳/inode |
| `json-get FILE POINTER` | RFC 6901 JSON Pointer，输出规范 JSON；缺失值报错 |
| `diff A B` | 统一 diff，相同退出 0，不同退出 1 |

学习路线的 index.md 定义 fail、expect_exit、expect_refused、snapshot、assert_unchanged、assert_same_tree、assert_content、assert_absent 及 JSON/fixture 函数。全部 Markdown 就是完整输入，函数没有藏在另一套 shell 文件中。

仅检查进程退出 77 表示 SKIP；`expect_exit 77 CMD` 验证产品退出码不会跳过 case。SKIP 原因来自检查输出，受 case/准备文档审核约束。

## 6. 约定与运行时输入

- 命令行必须明确提供一个或多个 Markdown 文件/目录，不推断默认路径。目录递归读取 Markdown，忽略隐藏目录与 README.md；重叠路径的同一文件只加载一次，不同文件重复 ID 报错。
- 单文件运行只读取该文件的 case 及其同目录 index.md 准备步骤，不加载其他章节的 case。
- 默认 PATH 提供产品命令；需要指定构建产物时用 `--subject-bin PATH` 或 `SEMSPEC_SUBJECT_BIN`，runner 校验后提供绝对 `SUBJECT_BIN`。不使用该变量的 case 无需配置待测程序。
- 默认每个 case 最多运行 180 秒；`--timeout` 设置默认值，case 注释中的 timeout 优先。当前平台直接使用 linux/macos 名称，不要求平台映射配置。
- 环境继承调用者，runner 提供 CASE_ROOT、WS、SEMSPEC_BIN、SEMSPEC_PROJECT_ROOT；SUBJECT_BIN 只在显式指定时提供。参数不从配置文件读取。
- 输入目录（文件取父目录）的最近公共目录是审核根；准备文档相对路径及 `REVIEWED.toml` 都以它为基准。单输入仍使用原目录。没有台账就是 UNREVIEWED；台账是人工审核输出，不是执行配置，init/lint/run 不创建它。

## 7. CLI

```text
semspec init DIR                     在指定目录创建 index.md 和 example.md，不创建配置、脚本或台账
semspec list PATH... [--domain D]     从文档发现 case 和审核状态
semspec show ITEM PATH...            当前全文、摘要、依赖与审核状态
semspec lint PATH...                  结构检查与 Bash 语法检查，不执行规格
semspec review PATH... [--strict]            待审项在 strict 模式下退出 1
semspec run PATH... [OPTIONS]         执行全部或指定 Markdown 文件/目录
    --case ID,... --domain D         选择 case；显式 ID 必须唯一且属于所选域
    --subject-bin PATH --timeout T  运行时待测程序和默认超时
    --keep --require-reviewed       保留现场、人工审核门禁
    --require-pass                  每条选定 case 必须 PASS，SKIP/XFAIL 也使门禁失败
    --format human|json --output F  报告输出
semspec --spec-dir PATH COMMAND      显式输入，可重复；人工 approve 使用此选项
semspec --spec-dir PATH approve ITEM... --reviewer NAME
                                     仅人工交互批准
semspec helper NAME [ARGS...]        setup、tree-state、json-get、diff
```

| runner 退出码 | 含义 |
|---|---|
| 0 | 无失败，SKIP/XFAIL 不算失败 |
| 1 | FAIL/XPASS、报告清单不完整，或 require-pass 下存在非 PASS，或 require-reviewed 下存在待审项 |
| 2 | 用法或规格错误，包括显式待测程序不可用 |
| 3 | ERROR，引擎错误，优先于 1 |

77 是 Bash 检查的退出码；runner 报告 SKIP 后仍按上表返回。SSH 签名、JUnit、并行执行仍不支持。

## 8. Digest 规范

```text
normalize(text)  = 每行去除行尾空白；去除末尾空行；以单个 '\n' 结尾；不做 Unicode 规范化
case_digest      = SHA256("semspec/case/v1\0" || normalize(case_text)
                          || "\0" || join(sorted(vocab_digests), "\n")
                          || "\0" || ENGINE_SEMANTICS)
vocab_digest     = SHA256("semspec/vocab/v1\0" || file_name || "\0" || normalize(file_text))
engine_digest    = SHA256("semspec/engine/v1\0" || ENGINE_SEMANTICS)
```

- `ENGINE_SEMANTICS` 是引擎源码里的常量字符串（如 `"1"`）。只有当执行语义变化时才升级：
  helper 输出格式、前导脚本、verdict 判定、digest 规范。普通重构和发布不升级它。
- case digest 包含它依赖的词汇和引擎版本，所以台账只需要为每个 case 存一个 digest，
  就能表达"在这套词汇和这个引擎下，文字与检查得到了批准"。
- 词汇和引擎也单独作为 sealed item 列在台账中，便于审核者分别批准。

## 9. 台账

```toml
format = 1

[[approval]]
item = "S-STAGE-001"
digest = "sha256:完整的64位十六进制摘要"
reviewer = "真实审核人"
date = "2026-10-02"
```

无批准为 UNREVIEWED，摘要相同为 REVIEWED，不同为 STALE。
词汇 item 为 `@vocab:文件名`，引擎 item 为 `@engine`。
批准不依赖测试通过；测试通过不会更新台账。批准台账用文件锁及原子替换避免丢失更新。

## 10. 执行语义

当前 `ENGINE_SEMANTICS=6`。以下变更需要升级该版本并更新摘要稳定性测试：

1. 每个 case 创建独立 CASE_ROOT，空 ws/ 是 cwd；stdin 为 /dev/null。
2. 子进程独立进程组；超时 TERM，5 秒后 KILL。正常退出也清理剩余子孙进程。
   脱离进程组的进程不受此机制管理，规格不得启动持久脱离进程。
3. 无超时时，0 为成功，77 为 SKIP；其余非零、信号或超时为失败。
4. SKIP 优先于 xfail。预期失败平台上，成功为 XPASS，失败为 XFAIL；启动错误为 ERROR。
5. FAIL/XFAIL/XPASS/ERROR 保留现场及日志；其余自动删除，除非指定 --keep。
6. 执行结果和审核独立；require-reviewed 检查全部所用对象，包括跳过的 case。
7. runner 报告必须包含选定 ID 的精确集合，每个只出现一次，空报告、漏项、重复或多余结果都使门禁失败。选定集合来自相同 Markdown 解析结果，不另建解析脚本。
8. `--require-pass` 要求所有选定 case 都是 PASS；SKIP/XFAIL 保留原 verdict，但退出 1，ERROR 仍优先退出 3。审核状态与此门禁独立。
9. 指定 `--output` 时，选择校验通过后、执行前删除旧报告，再原子发布本次报告。非法选择保留旧报告；启动失败或中断不会留下旧成功结果。报告不能覆盖已加载的 case、准备文档或审核台账。


## 11. 实现与验证

`tools/semspec` 是独立 Cargo 包，model/parse/seal/ledger/project/runner/helpers
及 CLI 分模块；不另建工作区子 crate 或执行器抽象。常规测试可以由 AI 维护。

```sh
just test-semspec
just semspec lint docs/src/zh/cases
just semspec lint docs/src/zh/reference
just semspec --spec-dir tools/semspec/semantics lint
```

自举规格在 tools/semspec/semantics/review.md，必须人工审核。
S-REVIEW-004 涉及 approve 的拒绝路径，AI 不得执行；lint 和常规 runner 测试可运行。

## 12. pVisor 首个语义域：Stage / Apply / Drop

第一版计划包含以下 case。「现状」列是 2026-10-01 在 macOS（macFUSE）上实测的结果；
正式语义以审核后的规格文本为准。

| ID | 性质 | 现状 |
|---|---|---|
| S-STAGE-001 | 暂存的 Job 结束后、apply 前，工作区逐项不变（包括非零退出和被信号终止） | 符合 |
| S-STAGE-002 | Job 内的读取能看到自己的写入、删除和重命名 | 待测 |
| S-STAGE-003 | review 清单精确等于对工作区的净效果；创建后又删除的临时文件不出现在清单里 | 待测 |
| S-STAGE-004 | stage 后 `apply --all` 的结果，与同一脚本直接在工作区副本中运行的结果相同（核心等价律） | 待测 |
| S-STAGE-005 | drop 之后工作区与 Job 前相同；此后 apply 必须被拒绝，且不改变工作区 | 符合 |
| S-STAGE-006 | `apply --path P` 只改变 P 及其后代；分两次选择性 apply 的结果等于一次 `--all` | 部分符合 |
| S-STAGE-007 | 重复 apply 同一路径不改变工作区 | 符合（第二次报告 already applied） |
| S-STAGE-008 | 目标在 staging 之后被外部修改、新建或删除时，apply 被拒绝，外部内容保留 | 符合 |
| S-STAGE-009 | 选中集合中任一路径冲突时，本次 apply 不改变任何路径 | 符合 |
| S-STAGE-010 | 外部修改 Agent 未触碰的路径，不影响 apply，且这些修改被保留 | 符合 |
| S-STAGE-011 | Agent 删除目录后，外部向该目录新增了文件，此时 apply 必须拒绝 | 符合 |
| S-STAGE-012 | apply 只写入目标工作区；即使路径被替换为指向外部的符号链接，也不写到外部 | 符合（以冲突拒绝） |
| S-STAGE-013 | stage 中的文件系统调用，报告结果与实际效果一致：失败则无效果，成功则有效果 | **违反**（macOS） |
| S-STAGE-014 | 可执行位和符号链接目标随 apply 原样落地，符号链接不被解引用 | 符合 |

S-STAGE-013 的现象：在 macOS 暂存工作区中执行 `ln -s /etc/hosts link`，`ln` 报
`Operation not permitted` 并以 1 退出，但链接实际已经创建，之后还能被 apply 到工作区。
overlay-core 的 `create_symlink` 本身成功，错误出现在 FUSE 回复之后。这条 case 将以
`xfail-on=macos` 登记，直到修复。

另外，`status --json` 的 `sample_paths` 中出现了内部 whiteout 名称 `.wh.d`，泄露了
内部表示。是否把"审查输出不暴露内部表示"列为语义，留给审核者决定。

## 13. 文档规格审核

`docs/src/zh/reference/cases.md` 是 DOC 规格源。修改语义、命令、断言或依赖词汇时，
先 lint，再运行受影响的 case。摘要变化使已有批准 STALE，PASS 不会重新批准。
人工审查差异和新语义后才更新台账；发布门禁要求真实人工批准。
AI 不得执行 approve 或编辑真实 REVIEWED.toml、遗留 .approved 文件。

## 14. 旧配置退场

不再支持 `--config`、配置文件、标题发现或 `semantic-case` 注解。STAGE 检查迁入
`docs/src/zh/cases/06-stage-apply.md`；DOC、VM 和 runner 自举规格统一使用代码块前的
`semspec: case` 注释及 Markdown 准备块。原陈述、检查和 xfail 保持完整；迁移回归
固定核对 74 条原有产品检查的内容摘要，不代替人工审核。引擎语义升级为 6，已有
审核须人工重审；迁移不改写真实台账或快照。`@vocab:` 和 JSON 的 `vocab_review`
保留为审核记录的字段名，其内容现在指向准备文档。
