# pVisor 语义规格

`stage-apply.md` 是按 docs/semspec-design.md §12 起草的 14 条 Stage/Apply/Drop
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
namespace；缺少条件报告 SKIP。清单词汇使用 python3 解析公开 status JSON。
S-STAGE-013 的 macOS xfail 来自设计中的已知问题；修复后 XPASS 要求重新审核该注解。

人工逐条检查陈述、违反示例及 Bash 检查，另行审核 core.sh、pvisor.sh 和引擎实现。
然后由人执行交互批准：

```sh
just semspec show S-STAGE-001
just semspec approve @engine @vocab:core.sh @vocab:pvisor.sh S-STAGE-001 --reviewer YOUR_NAME
just semspec diff S-STAGE-001
just semspec revoke S-STAGE-001 --reviewer YOUR_NAME --reason '需要重新讨论语义'
```

仓库尚未指定语义审核人的 GitHub 身份；维护者应在 CODEOWNERS 中为规格、配置、
词汇、引擎、REVIEWED.toml 和 .approved/ 指定真实人工审核者并开启分支保护。
完整审核后再以 `review --strict` 和 `run --require-reviewed` 作为发布门禁。
不要自动填充台账，也不要因为实现失败而削弱性质。删除已分配 case 时将 ID 放入
semspec.toml 的 retired，禁止复用。引擎执行语义变化必须升级 ENGINE_SEMANTICS。

## 文档场景迁移

`docs/src/zh/reference/cases.md` 同时是用户文档和 DOC 规格源，覆盖原 A01–M02 中仍有效的 54 个场景，`just cases` 只运行 DOC 域。
L01、L02 随 `env` 功能移除而退役，S-DOC-053、S-DOC-054 已登记在 retired 中。

```sh
just semspec list --domain DOC
just semspec run docs/src/zh/reference/cases.md --subject-bin target/release/pvisor
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
| L01 | S-DOC-053 (retired: env removed) |
| L02 | S-DOC-054 (retired: env removed) |
| M01 | S-DOC-055 |
| M02 | S-DOC-056 |
