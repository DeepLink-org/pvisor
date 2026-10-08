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
| Vocabulary | 项目的 Bash 函数文件，供检查复用断言及前提判断 |
| Subject | 被测程序、环境变量和超时配置 |
| Digest | case、词汇或引擎的规范化 SHA-256 摘要 |
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

## 4. Spec 文件格式

~~~~markdown
### S-STAGE-001：暂存的 Job 不改变工作区

**语义**：暂存的 Job 结束后、apply 前，工作区的完整状态与 Job 开始前相同。

**理由**：先审查再落地要求写入不穿透到工作区。

**违反示例**：删除直接作用于 lower，或清理路径自动合并 upper。

```bash
require_stage
printf orig > keep.txt
snapshot before .
expect_exit 3 pvisor --stage "$CASE_ROOT/stage" -- /bin/sh -c 'printf new > new.txt; exit 3'
assert_unchanged before .
```
~~~~

1. 三级标题格式为 `### S-<DOMAIN>-<NNN>：标题`，可用半角冒号，ID 在项目内唯一。
2. case 到下一个一至三级标题前结束，代码块里的标题不计。
3. 必须有非空 `**语义**`、`**违反示例**` 段落和恰好一个 `bash` 检查块。
4. 可选单行注解 `<!-- semantic-case: key=value ... -->`，按 shell 规则解析。

| 注解 | 含义 |
|---|---|
| `xfail-on` | 逗号分隔的平台名或 `all` |
| `xfail-reason` | 与 xfail-on 同时出现，描述已知违反及跟踪信息 |
| `vocab` | 逗号分隔的词汇文件名，覆盖默认词汇集合 |

前提条件写在普通 Bash 中，不使用 `requires` 注解或 probe 配置。删除的 ID 不复用，
历史以 Git 为准，runner 不另建退役清单。

## 5. Bash 与断言词汇

检查在 `set -euo pipefail` 下执行，词汇按文件名排序，从参与摘要的规范化字节
复制后 source。检查本身可以使用 `source` 或 `.`；被引用文件的审查由项目负责，
自动摘要仅绑定 case 和声明的词汇。语法校验用 `bash -n`，不依赖 tree-sitter。

前提判断复用普通 Bash 函数。例如：

```bash
skip() { printf 'SKIP: %s\n' "$*" >&2; exit 77; }
require_python3() { python3 --version >/dev/null 2>&1 || skip 'python3 unavailable'; }
```

仅检查进程的退出码 77 表示 SKIP；在检查中用 `expect_exit 77 CMD` 验证产品退出码
不会跳过 case。SKIP 原因来自检查输出。前提判断也受 case/词汇审核约束。

| 内置 helper | 行为 |
|---|---|
| `tree-state DIR` | 排序记录路径、类型、权限、完整内容 SHA-256、链接目标，不跟随链接，不含时间戳/inode |
| `json-get FILE POINTER` | RFC 6901 JSON Pointer，输出规范 JSON；缺失值报错 |
| `diff A B` | 统一 diff，相同退出 0，不同退出 1 |

`core.sh` 提供 fail、expect_exit、expect_refused、snapshot、assert_unchanged、
assert_same_tree、assert_content、assert_absent。这里的 snapshot 是检查工作区状态，
与已删除的批准文本快照机制无关。pVisor 的 pvisor.sh 提供领域断言和前提函数。

## 6. 配置：semspec.toml

```toml
[project]
name = "pvisor"
spec_dirs = ["tests/semantics"]
ledger = "tests/semantics/REVIEWED.toml"

[subject]
bin = "target/debug/pvisor"
language = "bash"
vocab = ["tests/semantics/vocab/core.sh", "tests/semantics/vocab/pvisor.sh"]
env = { RUST_LOG = "warn" }
timeout = "180s"

[platforms]
macos = { os = "macos" }
linux = { os = "linux" }
```

配置不参与摘要；词汇集合决定 case 摘要。当前 OS 必须恰好匹配一个平台。
环境变量继承调用者再叠加配置与 runner 变量；配置不能覆盖 SEMSPEC_*、CASE_ROOT、
WS、SUBJECT_BIN。仅支持 Bash；签名、JUnit 和并行尚未实现，不接受伪装成功。

## 7. CLI

```text
semspec init                         创建配置、示例、core.sh 和空台账
semspec list [--domain D]            列出 case 和审核状态
semspec show ITEM                    当前全文、摘要、依赖与审核状态
semspec lint                         解析、结构检查和 Bash 语法检查，不执行规格
semspec review [--strict]            待审项在 strict 模式下退出 1
semspec run [FILE.md] [OPTIONS]      执行全部或指定 Markdown 文件的 case
    --case ID,... --domain D        选择交集
    --subject-bin PATH              覆盖被测程序（或 SEMSPEC_SUBJECT_BIN）
    --keep --require-reviewed       保留现场、审核门禁
    --format human|json --output F  报告输出
semspec approve ITEM... --reviewer NAME
                                     仅人工交互批准
semspec helper NAME [ARGS...]        tree-state、json-get、diff
```

| runner 退出码 | 含义 |
|---|---|
| 0 | 无失败，SKIP/XFAIL 不算失败 |
| 1 | FAIL/XPASS，或 require-reviewed 下存在待审项 |
| 2 | 用法、配置或规格错误，包括被测程序不可用 |
| 3 | ERROR，引擎错误，优先于 1 |

77 是 Bash 检查的退出码，runner 报告 SKIP 后仍按上表返回。

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

当前 `ENGINE_SEMANTICS=2`。以下变更需要升级该版本并更新摘要稳定性测试：

1. 每个 case 创建独立 CASE_ROOT，空 ws/ 是 cwd；stdin 为 /dev/null。
2. 子进程独立进程组；超时 TERM，5 秒后 KILL。正常退出也清理剩余子孙进程。
   脱离进程组的进程不受此机制管理，规格不得启动持久脱离进程。
3. 无超时时，0 为成功，77 为 SKIP；其余非零、信号或超时为失败。
4. SKIP 优先于 xfail。预期失败平台上，成功为 XPASS，失败为 XFAIL；启动错误为 ERROR。
5. FAIL/XFAIL/XPASS/ERROR 保留现场及日志；其余自动删除，除非指定 --keep。
6. 执行结果和审核独立；require-reviewed 检查全部所用对象，包括跳过的 case。

## 11. 实现与验证

`tools/semspec` 是独立 Cargo 包，model/config/parse/seal/ledger/project/runner/helpers
及 CLI 分模块；不另建工作区子 crate 或执行器抽象。常规测试可以由 AI 维护。

```sh
just test-semspec
just semspec lint
just semspec --config semspec-doc.toml lint
just semspec --config tools/semspec/semantics/semspec.toml lint
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
