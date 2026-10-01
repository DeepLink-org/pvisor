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
