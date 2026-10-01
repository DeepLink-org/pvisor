# semspec 设计：人工审核的语义规格测试

semspec 是一个独立的 Rust 命令行工具，用于维护和执行**语义规格**（semantic
specification）：项目必须保持的行为性质，用人能读懂的文字陈述，并配一段黑盒检查。
它和单元测试分工不同：

| | 单元测试 / 集成测试 | 语义规格（semspec） |
|---|---|---|
| 回答的问题 | 代码是否按实现者的意图工作 | 系统是否满足项目承诺的语义 |
| 作者 | 可以交给 AI 编写与维护 | AI 可以起草，必须由人审核 |
| 被测对象 | 内部函数、模块 | 真实的产品入口（CLI、API），黑盒 |
| 修改规则 | 随实现自由调整 | 任何修改都会使审核失效，需要人重新批准 |
| 失败的含义 | 实现有缺陷 | 产品违反承诺；要么修复实现，要么由人修改承诺 |

pVisor 是第一个使用方，首个语义域是 Stage/Apply/Drop。本文以 pVisor 为例说明，
但工具本身不依赖 pVisor。

## 1. 目标与非目标

**目标**

1. 规格的权威文本是 Markdown：性质陈述、理由、违反示例和检查脚本放在同一段落里，
   审核者阅读的就是被执行的内容。
2. 审核可验证：每个被审核对象都有一个规范化摘要（digest），审核台账记录"谁在何时
   批准了哪个摘要"。文本、断言词汇或引擎语义发生变化后，审核状态自动变为 STALE。
3. 断言词汇是封闭的小集合，本身也是审核对象。修改 `assert_unchanged` 的含义，和修改
   每一个用到它的 case 一样，都需要人重新批准。
4. 黑盒执行：每个 case 在独立的临时目录中运行，只通过被测系统的公开入口交互。
5. 可以直接放进 CI：输出人类可读格式、JSON 和 JUnit；退出码语义稳定。
6. 能诚实地表达"已知违反"（XFAIL）：语义不因实现暂时做不到而被改写。

**非目标**

- 不是通用测试框架，不替代 cargo test、pytest。
- 不做形式化验证；检查是对性质的有限采样，PASS 不等于证明。
- 不负责防御拥有仓库写权限的恶意人类。它防御的是"AI 或自动化流程无意或有意地削弱
  语义"，以及审核信息的漂移。
- 第一版只支持 bash 检查脚本。

## 2. 核心概念

| 概念 | 含义 |
|---|---|
| Spec 文件 | `*.md` 文件，包含若干语义 case，按语义域组织（如 `stage-apply.md`） |
| Case | 一条语义性质，由 ID、标题、注解、规定的文字段落和一个检查块组成 |
| Claim | case 中的 `**语义**` 段落，是规格的权威陈述 |
| Check | case 中唯一的检查代码块，用于驱动被测系统并断言 |
| Vocabulary | 检查脚本可以调用的断言函数集合，由内置 helper 加项目词汇文件组成 |
| Subject | 被测系统的配置：二进制路径、环境变量和能力探测 |
| Requirement | case 运行的前提条件（如 `stage` 需要 FUSE），由 probe 判定；不满足时为 SKIP |
| Sealed item | 需要人审核的对象：case、词汇文件、引擎（engine） |
| Digest | 对 sealed item 规范化内容计算的 SHA-256，带域分隔前缀 |
| Ledger | 审核台账，记录每个 sealed item 被批准时的 digest、审核者、日期和可选签名 |
| Verdict | 一次执行的结论：PASS、FAIL、SKIP、XFAIL、XPASS、ERROR |
| Review state | REVIEWED、UNREVIEWED、STALE |

**信任链**：一个 PASS 值得信任，需要同时满足三个条件：case 文本是 REVIEWED；它用到的
每个词汇文件是 REVIEWED；当前引擎版本是 REVIEWED。任一不满足时，结论会标注
"unreviewed"，`--require-reviewed` 模式下视为失败。

## 3. 信任模型

### 3.1 威胁

| 威胁 | 例子 | 对策 |
|---|---|---|
| 削弱断言 | 把 `assert_unchanged` 改成只比较文件名 | 词汇文件是 sealed item；case digest 包含所用词汇的 digest |
| 改写语义迎合实现 | 实现做不到，于是把规格改成"apply 可以覆盖外部修改" | case 文本一改就是 STALE；CI 要求 REVIEWED |
| 绕过审核 | 直接改台账 | `approve` 只能在交互终端运行；可选 SSH 签名；台账受 CODEOWNERS 保护 |
| 用 xfail 掩盖回归 | 给失败的 case 加上 `xfail-on` | 注解属于 case 文本，修改同样需要重新审核 |
| 引擎行为漂移 | 新版 semspec 改变了 digest 规范或执行语义 | 引擎有 `ENGINE_SEMANTICS` 版本号，变化后所有依赖它的 case 都会 STALE |
| 检查与陈述不符 | 文字说"逐字节不变"，脚本只检查文件存在 | 这只能靠人审；工具负责把文字和脚本放在同一屏展示，并强制写出"违反示例" |

### 3.2 人和 AI 的分工

- AI 可以：起草新 case、修复实现直到 case 通过、为 runner 本身写单元测试、提出修改规格
  的建议（以 PR 描述的形式）。
- AI 不可以：运行 `semspec approve` 或 `revoke`、编辑台账、为了让 case 通过而修改
  `**语义**` 段落或检查脚本。
- 使用方仓库的 `AGENTS.md` 应写明这些规则；semspec 用工具约束为它们兜底，但不能完全
  替代流程。

### 3.3 签名（可选，v0.2）

批准时可以用 `ssh-keygen -Y sign` 对 `(item, digest, reviewer, date)` 的规范编码签名，
CI 用项目中的 `allowed_signers` 文件验证。签名缺失或无效的条目视为 UNREVIEWED。
这样，即使有人直接编辑台账，也无法伪造审核。

## 4. Spec 文件格式

### 4.1 结构

~~~~markdown
# Stage / Apply / Drop 语义

本文件的前言部分不属于任何 case，不参与 digest。

## 工作区隔离

### S-STAGE-001：暂存的 Job 不改变工作区

<!-- semantic-case: requires=stage -->

**语义**：使用 `--stage` 的 Job 结束后、apply 之前，工作区的完整状态（路径集合、
文件类型、内容、权限位、符号链接目标）与 Job 开始前完全相同。无论 Agent 成功退出、
非零退出还是被信号终止，这一点都成立。

**理由**：stage 的全部价值在于"先审查再落地"。只要有任何写入穿透到工作区，审查就失去意义。

**违反示例**：写入被正确暂存，但 `rm` 直接作用于 lower；Agent 被 SIGKILL 终止时，
清理路径把 upper 合并回了工作区。

```bash
printf orig > keep.txt; printf base > edit.txt; mkdir d; printf x > d/x
snapshot before .
expect_exit 3 pvisor --stage "$CASE_ROOT/stage" -- /bin/sh -c \
  'printf new > new.txt; printf changed > edit.txt; rm -rf d; exit 3'
assert_unchanged before .
```
~~~~

### 4.2 规则

1. case 标题使用三级标题：`### <ID>：<标题>`，也可以用半角冒号。ID 的格式是
   `S-<DOMAIN>-<NNN>`，整个项目内唯一。
2. case 的范围从标题行开始，到下一个一至三级标题之前结束；代码块里的 `#` 不算标题。
3. 每个 case 必须包含 `**语义**` 和 `**违反示例**` 段落；`**理由**` 是推荐段落。
   缺少必需段落时属于解析错误。
4. 每个 case 有且只有一个检查块，其语言标签必须是 Subject 支持的语言（第一版只支持 `bash`）。
5. 注解最多一行，格式为 `<!-- semantic-case: key=value ... -->`，值按 shell 规则解析：

| 键 | 值 | 含义 |
|---|---|---|
| `requires` | 逗号分隔的 requirement 名 | 全部满足才运行，否则为 SKIP |
| `xfail-on` | 逗号分隔的平台名或 `all` | 在这些平台上，预期检查失败 |
| `xfail-reason` | 字符串 | 必须和 `xfail-on` 同时出现，说明违反的原因及跟踪链接 |
| `vocab` | 逗号分隔的词汇文件名 | 覆盖配置中的默认词汇集合 |

6. ID 一经分配就不再复用。删除的 case 应在 `retired` 列表中登记（见 §6），防止被同一
   ID 的新文本悄悄替换。

## 5. 断言词汇

词汇由两层组成。

**内置 helper**：由 semspec 二进制以 Rust 实现，通过 `semspec helper <name>` 调用。
它们属于引擎，受 `ENGINE_SEMANTICS` 版本约束：

| helper | 输出 / 行为 |
|---|---|
| `tree-state DIR` | 规范化的目录状态，每行一个条目：`dir MODE PATH`、`file MODE PATH SHA256`、`link PATH -> TARGET`、`other KIND PATH`；按路径排序，不跟随符号链接，不包含 mtime、atime、inode 号 |
| `json-get FILE POINTER` | 按 RFC 6901 JSON Pointer 取值，原样输出 |
| `diff A B` | 统一 diff；不同时以退出码 1 结束 |

**项目词汇文件**：bash 函数库，例如 `tests/semantics/vocab/core.sh`、`pvisor.sh`。
它们本身是 sealed item。semspec 提供一份标准的 `core.sh` 模板，由 `semspec init`
写入使用方仓库后归使用方所有：

| 函数 | 语义 |
|---|---|
| `fail MSG` | 以 `SEMANTIC VIOLATION: MSG` 失败 |
| `expect_exit N CMD...` | CMD 的退出码必须恰好为 N |
| `expect_refused CMD...` | CMD 必须以非零退出码结束 |
| `snapshot NAME DIR` | 保存 `tree-state DIR` |
| `assert_unchanged NAME DIR` | 当前 `tree-state DIR` 与快照完全相同 |
| `assert_same_tree A B` | 两棵树的 `tree-state` 相同 |
| `assert_content PATH TEXT` | PATH 是普通文件（不是链接），内容恰好为 TEXT |
| `assert_absent PATH` | PATH 不存在，悬空链接也算存在 |

pVisor 的 `pvisor.sh` 补充领域词汇：

| 函数 | 语义 |
|---|---|
| `pvisor ARGS...` | 调用被测二进制 `$SUBJECT_BIN` |
| `review_changes STAGE` | 输出 `status --review --json` 中每个变更的 `<kind> <path>`，排序 |
| `assert_changes STAGE` | 标准输入给出的期望变更清单与 `review_changes` 完全相同 |

检查脚本在 `set -euo pipefail` 下运行，前导部分按顺序 source 词汇文件。检查脚本不能
自行 source 其他文件；解析器拒绝 `source`、`.` 以及对 `$SEMSPEC_*` 变量的赋值。
这是为了让审核范围保持封闭，它不是安全沙箱。

## 6. 配置：`semspec.toml`

放在使用方仓库的根目录：

```toml
[project]
name = "pvisor"
spec_dirs = ["tests/semantics"]
ledger = "tests/semantics/REVIEWED.toml"
approved_snapshots = "tests/semantics/.approved"   # 已批准文本的快照，供 diff 使用
retired = ["S-STAGE-099"]

[subject]
bin = "target/debug/pvisor"          # 可用 --subject-bin 或 SEMSPEC_SUBJECT_BIN 覆盖
language = "bash"
vocab = ["tests/semantics/vocab/core.sh", "tests/semantics/vocab/pvisor.sh"]
env = { RUST_LOG = "warn" }
timeout = "180s"

[platforms]                          # 平台名 → 判定条件
macos = { os = "macos" }
linux = { os = "linux" }

[requirements.stage]                 # requirement 由 probe 判定
macos = { path_exists = "/Library/Filesystems/macfuse.fs" }
linux = { all = [
  { path_exists = "/dev/fuse" },
  { command_succeeds = ["unshare", "--user", "--map-root-user", "--mount", "true"] },
] }

[signing]                            # v0.2
required = false
allowed_signers = "tests/semantics/allowed_signers"
```

配置文件本身不是 sealed item，因为它描述的是环境，而不是语义。但 `vocab` 列表决定了
case digest 的组成（§8），所以改动词汇集合同样会让相关 case 变为 STALE。

## 7. 命令行接口

```text
semspec init                         生成 semspec.toml、示例 spec、core.sh、空台账
semspec list [--domain D]            列出 case 及其审核状态
semspec show <ID>                    展示 case 全文、digest、依赖的词汇及审核信息
semspec lint                         只解析和校验，不执行（适合 pre-commit）
semspec run [OPTIONS]                执行 case
    --case <ID,...>  --domain <D>    选择范围
    --subject-bin <PATH>             覆盖被测二进制
    --jobs <N>                       并行度，默认 1（被测系统可能有全局状态）
    --keep                           保留所有 case 目录；失败时总是保留
    --require-reviewed               未审核或 STALE 的项计为失败
    --format human|json|junit        输出格式；--output <FILE>
semspec review [--strict]            台账状态总览；--strict 下有待审项时退出码为 1
semspec diff <ID|@vocab:NAME|@engine>  当前文本与上次批准文本的差异
semspec approve <ITEM>... --reviewer <NAME> [--sign <KEY>]
                                     交互式批准：逐项展示全文或 diff，要求输入 ITEM 确认
semspec revoke <ITEM>... --reviewer <NAME> --reason <TEXT>
semspec helper <NAME> [ARGS...]      内置 helper，供词汇文件调用
```

**退出码**

| 码 | 含义 |
|---|---|
| 0 | 全部通过（SKIP 和 XFAIL 不算失败） |
| 1 | 存在 FAIL、XPASS，或在 `--require-reviewed` 下存在待审项 |
| 2 | 用法错误、配置错误或 spec 解析错误 |
| 3 | 引擎内部错误（ERROR verdict） |

`approve` 和 `revoke` 在 stdin 不是 TTY 时直接以退出码 2 拒绝执行。批准 STALE 项时默认
展示 diff，而不是全文，以降低重复审核的成本。

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

## 9. 台账格式：`REVIEWED.toml`

```toml
# 由 `semspec approve` 写入。请勿手工编辑。
format = 1

[[approval]]
item = "S-STAGE-001"
digest = "sha256:3f1c…"
reviewer = "reiase"
date = "2026-10-02"
signature = "-----BEGIN SSH SIGNATURE-----\n…"   # 可选

[[approval]]
item = "@vocab:core.sh"
digest = "sha256:9ab2…"
reviewer = "reiase"
date = "2026-10-02"

[[revocation]]
item = "S-STAGE-004"
reviewer = "reiase"
date = "2026-10-05"
reason = "rename 的语义需要重新讨论"
```

每次批准时，semspec 会把已批准的规范化文本写入 `.approved/<item>.md`，供 `semspec diff`
使用。快照只是审核辅助，不参与判定；判定只依赖 digest。

## 10. 主要数据类型

```rust
// semspec-core::model
pub struct CaseId(String);                 // 已校验：S-<DOMAIN>-<NNN>

pub struct SpecFile {
    pub path: PathBuf,                     // 相对项目根目录
    pub cases: Vec<Case>,
}

pub struct Case {
    pub id: CaseId,
    pub title: String,
    pub domain: String,                    // 由 ID 中的 DOMAIN 得出
    pub span: Span,                        // 源文件及行范围，用于报错和展示
    pub text: NormalizedText,              // 参与 digest 的完整 case 文本
    pub prose: Prose,
    pub check: Check,
    pub annotation: Annotation,
}

pub struct Prose {
    pub claim: String,                     // **语义**
    pub violation: String,                 // **违反示例**
    pub rationale: Option<String>,         // **理由**
}

pub struct Check {
    pub language: Language,                // 第一版只有 Language::Bash
    pub source: String,
    pub span: Span,
}

pub struct Annotation {
    pub requires: BTreeSet<RequirementName>,
    pub xfail: Option<ExpectedFailure>,
    pub vocab: Option<Vec<VocabName>>,
}

pub struct ExpectedFailure {
    pub platforms: PlatformSet,            // 支持 `all`
    pub reason: String,
}

pub struct NormalizedText(String);         // 构造时完成规范化，保证不变式
pub struct Span { pub file: PathBuf, pub start_line: u32, pub end_line: u32 }
```

```rust
// semspec-core::seal
pub enum SealedItem {
    Case(CaseId),
    Vocab(VocabName),                      // 显示为 @vocab:core.sh
    Engine,                                // 显示为 @engine
}

pub struct Digest([u8; 32]);               // 显示为 sha256:<hex>

pub trait Sealable {
    fn item(&self) -> SealedItem;
    fn digest(&self, ctx: &SealContext) -> Digest;
    fn review_text(&self) -> &NormalizedText;
}

pub struct SealContext<'a> {
    pub vocab: &'a [Vocab],
    pub engine_semantics: &'static str,
}
```

```rust
// semspec-core::ledger
pub struct Ledger {
    pub approvals: BTreeMap<SealedItem, Approval>,
    pub revocations: Vec<Revocation>,
}

pub struct Approval {
    pub digest: Digest,
    pub reviewer: String,
    pub date: chrono::NaiveDate,
    pub signature: Option<SshSignature>,
}

pub enum ReviewState {
    Reviewed { by: String, on: NaiveDate },
    Unreviewed,
    Stale { approved: Digest },            // 文本、词汇或引擎发生变化
    Revoked { reason: String },
    BadSignature,                          // 签名要求开启且验证失败
}

impl Ledger {
    pub fn state(&self, item: &SealedItem, current: &Digest, policy: &SigningPolicy) -> ReviewState;
    pub fn approve(&mut self, item: SealedItem, approval: Approval);
    pub fn revoke(&mut self, item: SealedItem, revocation: Revocation);
}
```

```rust
// semspec-core::verdict
pub enum Verdict {
    Pass,
    Fail { output_tail: String },
    Skip { requirement: RequirementName, reason: String },
    XFail { reason: String },
    XPass,                                 // 已知违反消失：应由人移除 xfail 注解
    Error { message: String },             // 引擎自身失败，与语义无关
}

pub struct CaseResult {
    pub id: CaseId,
    pub verdict: Verdict,
    pub review: ReviewState,
    pub duration: Duration,
    pub workdir: Option<PathBuf>,          // 保留时记录
}

pub struct RunReport {
    pub engine_semantics: &'static str,
    pub engine_review: ReviewState,
    pub vocab_review: Vec<(VocabName, ReviewState)>,
    pub platform: PlatformName,
    pub results: Vec<CaseResult>,
}

impl RunReport {
    pub fn exit_code(&self, require_reviewed: bool) -> ExitCode;
}
```

```rust
// semspec-runner
pub trait Executor {                       // 每种检查语言一个实现
    fn language(&self) -> Language;
    fn run(&self, plan: &ExecutionPlan) -> Result<Execution, EngineError>;
}

pub struct ExecutionPlan {
    pub script: String,                    // 前导 + 词汇 + 检查脚本
    pub workdir: PathBuf,                  // $CASE_ROOT/ws，同时是 cwd
    pub case_root: PathBuf,                // $CASE_ROOT
    pub env: BTreeMap<String, String>,     // SUBJECT_BIN、CASE_ROOT、WS、SEMSPEC_BIN 及配置项
    pub timeout: Duration,
}

pub struct Execution {
    pub status: ExitStatus,
    pub timed_out: bool,
    pub log: PathBuf,                      // stdout 与 stderr 合并后的日志
}

pub trait Probe {                          // requirement 判定
    fn check(&self) -> Result<(), String>; // Err 为 SKIP 原因
}

pub trait Reporter {
    fn case_finished(&mut self, result: &CaseResult);
    fn finish(&mut self, report: &RunReport) -> io::Result<()>;
}
```

**执行语义**（受 `ENGINE_SEMANTICS` 约束）：

1. 为每个 case 创建临时目录 `$CASE_ROOT`，其中 `ws/` 为空工作区，也是 cwd。
2. 子进程放入新的进程组；超时后先发 SIGTERM，宽限 5 秒后发 SIGKILL 给整个进程组。
3. stdin 为 `/dev/null`；环境变量继承调用者环境，再叠加 plan 中的变量。
4. 退出码为 0 是检查通过；非 0 或超时是检查失败；无法启动解释器是 ERROR。
5. 检查结果再结合 xfail 注解得出最终 verdict：预期失败的平台上，失败为 XFAIL，通过为 XPASS。
6. FAIL、XPASS 和 ERROR 时保留 `$CASE_ROOT`；其他情况下，未指定 `--keep` 就删除。

## 11. 项目布局

### 11.1 semspec 仓库

```text
semspec/
├── Cargo.toml                      # workspace
├── README.md
├── LICENSE
├── crates/
│   ├── semspec-core/               # 纯逻辑，无进程、无终端交互
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── model.rs            # Case、Prose、Check、Annotation、CaseId
│   │       ├── parse.rs            # Markdown → SpecFile（pulldown-cmark，带行号）
│   │       ├── lint.rs             # 结构规则、禁止的 source、ID 唯一及 retired 检查
│   │       ├── normalize.rs        # NormalizedText
│   │       ├── seal.rs             # SealedItem、Digest、Sealable、ENGINE_SEMANTICS
│   │       ├── ledger.rs           # REVIEWED.toml 读写、ReviewState
│   │       ├── config.rs           # semspec.toml
│   │       ├── platform.rs         # 平台判定、PlatformSet
│   │       └── verdict.rs          # Verdict、CaseResult、RunReport、退出码
│   ├── semspec-runner/             # 执行与探测
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── bash.rs             # Executor for Bash：前导脚本、词汇拼接
│   │       ├── process.rs          # 进程组、超时、日志
│   │       ├── probe.rs            # path_exists、command_succeeds、all/any
│   │       ├── workspace.rs        # 临时目录生命周期
│   │       └── helpers/            # 内置 helper（tree-state、json-get、diff）
│   ├── semspec-sign/               # v0.2：调用 ssh-keygen -Y sign/verify
│   └── semspec-cli/                # 二进制 `semspec`
│       └── src/
│           ├── main.rs             # clap 定义
│           ├── cmd/                # init、list、show、lint、run、review、diff、approve、revoke、helper
│           ├── interactive.rs      # TTY 检测与确认
│           └── report/             # human.rs、json.rs、junit.rs
├── templates/                      # semspec init 使用
│   ├── semspec.toml
│   ├── example.md
│   └── vocab/core.sh
├── tests/
│   ├── fixtures/                   # 合法与非法 spec、台账样例
│   ├── parse.rs
│   ├── digest_stability.rs         # 固定输入 → 固定 digest，防止规范漂移
│   ├── ledger.rs
│   └── cli.rs                      # assert_cmd 端到端
└── semantics/                      # semspec 自己的语义规格（自举，见 §13）
    ├── semspec.toml
    ├── REVIEWED.toml
    └── review.md                   # 例如"修改 case 文本后状态必须是 STALE"
```

依赖选择：`clap`、`serde`、`toml`、`serde_json`、`pulldown-cmark`（解析时保留源位置）、
`sha2`、`thiserror`/`anyhow`、`tempfile`、`nix`（进程组与信号）、`chrono`、
`similar`（diff）、`quick-junit`；测试使用 `assert_cmd` 和 `insta`。

`semspec-core` 不依赖任何进程和终端相关的 crate，可以被其他工具（例如编辑器插件、
审核机器人）直接嵌入。

### 11.2 使用方仓库（以 pVisor 为例）

```text
pVisor/
├── semspec.toml
├── AGENTS.md                       # 写明 AI 不得运行 approve/revoke、不得编辑台账
├── .github/CODEOWNERS              # tests/semantics/** 需要指定人审核
└── tests/semantics/
    ├── README.md                   # 本项目的审核流程（不参与解析）
    ├── stage-apply.md              # 第一个语义域
    ├── vocab/
    │   ├── core.sh
    │   └── pvisor.sh
    ├── REVIEWED.toml
    ├── allowed_signers             # v0.2
    └── .approved/                  # 已批准文本快照
```

justfile 中增加：

```just
semantics *args: (build "debug")
    semspec run --subject-bin "{{ target_dir }}/debug/pvisor" "$@"
```

CI 中运行 `semspec lint`、`semspec review --strict` 和
`semspec run --require-reviewed --format junit`。在台账填满之前，`--require-reviewed`
可以先只用在已审核的语义域上。

## 12. pVisor 首个语义域：Stage / Apply / Drop

第一版计划包含以下 case。下文的"现状"是 2026-10-01 在 macOS（macFUSE）上实测的结果；
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

## 13. 自举与质量保证

- semspec 自身的代码正确性由常规测试保证：解析 fixture、digest 稳定性快照、台账往返、
  CLI 端到端测试。这部分可以交给 AI 维护。
- semspec 的核心承诺也写成 semspec 规格，放在 `semantics/review.md`。例如：修改 case
  的任意一个字节后，状态必须是 STALE；非 TTY 下 approve 必须被拒绝；修改词汇文件后，
  依赖它的 case 必须是 STALE；XPASS 必须导致非零退出码。这些规格由人审核。
- `digest_stability.rs` 固定一组输入和期望的 digest。只要修改规范化或 digest 逻辑
  而没有升级 `ENGINE_SEMANTICS`，这个测试就会失败。

## 14. 版本计划

| 版本 | 范围 |
|---|---|
| v0.1 | core、runner、cli；bash 执行器；`init`、`list`、`show`、`lint`、`run`、`review`、`diff`、`approve`、`revoke`、`helper`；human 和 JSON 输出；pVisor 的 S-STAGE 域上线 |
| v0.2 | SSH 签名与 `allowed_signers`；JUnit 输出；`--jobs` 并行；批准时默认展示 diff |
| v0.3 | 第二种检查语言（如 Python）；case 级依赖（一个 case 引用另一个 case 的前置状态）；覆盖率视图：哪些 CLI 命令或语义域没有 case |
| v1.0 | 格式与 `ENGINE_SEMANTICS=1` 冻结，发布到 crates.io |

## 15. 待决问题

1. **项目名**：暂用 `semspec`，发布前需要检查 crates.io 上是否重名。
2. **是否要求签名**：小团队初期可以只靠 TTY 加 CODEOWNERS；对外发布时建议开启签名。
3. **ID 是否带语义版本**：语义被有意改变时，是沿用 ID 重新审核，还是分配新 ID 并把旧
   ID 登记为 retired？当前倾向前者，由台账的历史记录变更。
4. **平台差异化语义**：同一性质在不同平台上的承诺不同时，是拆成两个 case，还是在注解
   中引入 `only-on`？当前倾向拆分，让每条语义陈述都无条件成立。
5. **原型处理**：仓库中的 `scripts/run-semantic-tests.py` 是本设计的早期原型，Rust
   版本可用之后删除，其中的 case 格式按本文 §4 迁移。
