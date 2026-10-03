# pVisor 语义规格

`stage-apply.md` 是按 tools/semspec/DESIGN.md §12 起草的 14 条 Stage/Apply/Drop
规格，尚未人工审核。PASS 表示当前样例满足检查，不表示规格已被批准。

```sh
just semspec lint
just semspec list --domain STAGE
just semantics --case S-STAGE-001
just semantics --format json --output target/semantics.json
just semspec review --strict
```

工作区、stage、个人配置和 Job 数据均位于临时 CASE_ROOT。失败保留现场，成功自动
删除；`--keep` 保留所有现场。macOS 需要 macFUSE，Linux 需要 /dev/fuse 和 user/mount
namespace；Bash 前提函数缺少条件时退出 77，报告 SKIP。清单词汇使用 python3 解析公开 status JSON。
S-STAGE-013 的 macOS xfail 来自设计中的已知问题；修复后 XPASS 要求重新审核该注解。

人工逐条检查陈述、违反示例及 Bash 检查，另行审核 core.sh、pvisor.sh 和引擎实现。
然后由人执行交互批准：

```sh
just semspec show S-STAGE-001
just semspec approve @engine @vocab:core.sh @vocab:pvisor.sh S-STAGE-001 --reviewer YOUR_NAME
```

仓库尚未指定语义审核人的 GitHub 身份；维护者应在 CODEOWNERS 中为规格、配置、
词汇、引擎和 REVIEWED.toml 指定真实人工审核者并开启分支保护。
首轮人工审核仅覆盖 14 条 S-STAGE 及其词汇和引擎。
完整审核后，以限定 STAGE 域的 `review --strict` 和 `run --require-reviewed` 作为发布门禁。
DOC 继续由同一 runner 执行示例回归，不作为已审核的语义承诺；其 case 与断言保持原样。
不要自动填充台账，也不要因为实现失败而削弱性质。已分配的 ID 不复用，
差异和删除历史用 Git 审查，不维护独立退役清单。引擎执行语义变化必须升级 ENGINE_SEMANTICS。

## 新 CLI 学习路线（USE）

[`docs/src/zh/cases/index.md`](../../docs/src/zh/cases/index.md) 将使用叙事和实际检查放在同一套文档中，
按首次运行、文件审查、workspace checkpoint/fork、访问边界、轨迹恢复逐步展开。
旧 DOC/STAGE 规格、词汇和入口保留；USE 使用独立的 `semspec-use.toml` 与 `journey.sh`。
`journey.sh` 内的 Python 断言和 fixture 服务一起参与词汇摘要，不在摘要外隐藏检查。

```sh
just semspec --config semspec-use.toml lint
just cases-v2
just cases-v2 --case S-USE-005,S-USE-007 --keep
```

USE 仍为 UNREVIEWED，不自动批准。`just cases-v2` 的执行门禁要求选定 ID 精确匹配、
全部 PASS，并拒绝 SKIP/XFAIL、空报告、漏项和重复项；可单独选择场景调试。
CI 的 Linux 隔离 job 执行全部 USE，预检 FUSE 和 user/mount/network namespace，
报告作为 `pvisor-learning-report` artifact 上传。

## 文档场景迁移

VM 新增六条 DOC 场景 S-DOC-057..062，见
[`cases-vm.md`](../../docs/src/zh/reference/cases-vm.md)。`just cases` 扫描整个 DOC
目录；`just vm-cases` 准备 SDK 驱动并执行这六条场景。需要 Linux/KVM 或
Apple Silicon/HVF、Linux guest rootfs；SDK 场景还需要 guest Python；S-DOC-062 还需要 FUSE/macFUSE kernel backend。
新规格与 `vm.sh` 保持 UNREVIEWED，没有更新人工审批台账。

`docs/src/zh/reference/cases.md` 同时是用户文档和 DOC 规格源，覆盖原 A01–M02 中仍有效的 54 个场景，`just cases` 只运行 DOC 域。
L01、L02 随 `env` 功能移除，S-DOC-053、S-DOC-054 的删除记录保留在 Git 中。

```sh
just semspec --config semspec-doc.toml list --domain DOC
just semspec --config semspec-doc.toml run docs/src/zh/reference/cases.md --subject-bin target/release/pvisor
just cases --case S-DOC-001,S-DOC-012 --keep
just cases
```

默认报告为 `target/pvisor-case-report.json`；自动 CI 选择的原场景集合保持不变。
8 条预期非零退出检查仍须通过原断言，不标记 xfail。
资源通过 `PVISOR_CASE_ROOTFS/IMAGE/CONTAINER_IMAGE/CONTAINER_RUNTIME/AGENT` 配置。
命令、退出预期及原断言均在 case digest 中，夹具和 JSON 断言由 sealed `cases.sh` 提供。

| 文档编号 | semspec ID |
|---|---|
| A01 | S-DOC-001 |
| A02 | S-DOC-002 |
| A03 | S-DOC-003 |
| A04 | S-DOC-004 |
| A05 | S-DOC-005 |
| A06 | S-DOC-006 |
| A07 | S-DOC-007 |
| B01 | S-DOC-008 |
| B02 | S-DOC-009 |
| B03 | S-DOC-010 |
| B04 | S-DOC-011 |
| C01 | S-DOC-012 |
| C02 | S-DOC-013 |
| C03 | S-DOC-014 |
| C04 | S-DOC-015 |
| C05 | S-DOC-016 |
| C06 | S-DOC-017 |
| D01 | S-DOC-018 |
| D02 | S-DOC-019 |
| D03 | S-DOC-020 |
| D04 | S-DOC-021 |
| D05 | S-DOC-022 |
| D06 | S-DOC-023 |
| E01 | S-DOC-024 |
| E02 | S-DOC-025 |
| E03 | S-DOC-026 |
| E04 | S-DOC-027 |
| E05 | S-DOC-028 |
| E06 | S-DOC-029 |
| F01 | S-DOC-030 |
| F02 | S-DOC-031 |
| F03 | S-DOC-032 |
| F04 | S-DOC-033 |
| G01 | S-DOC-034 |
| G02 | S-DOC-035 |
| G03 | S-DOC-036 |
| G04 | S-DOC-037 |
| G05 | S-DOC-038 |
| G06 | S-DOC-039 |
| G07 | S-DOC-040 |
| H01 | S-DOC-041 |
| H02 | S-DOC-042 |
| I01 | S-DOC-043 |
| I02 | S-DOC-044 |
| I03 | S-DOC-045 |
| J01 | S-DOC-046 |
| J02 | S-DOC-047 |
| J03 | S-DOC-048 |
| K01 | S-DOC-049 |
| K02 | S-DOC-050 |
| K03 | S-DOC-051 |
| K04 | S-DOC-052 |
| L01 | S-DOC-053 (env removed) |
| L02 | S-DOC-054 (env removed) |
| M01 | S-DOC-055 |
| M02 | S-DOC-056 |
