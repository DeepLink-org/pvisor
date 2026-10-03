# 在 CI 中运行 Agent

在 CI 中让 Agent 完成任务，把改动作为产物交给评审者。任务结束后上传 Stage 和运行证据；合入工作区由评审步骤决定。

先准备一个能运行 `pvisor run --safe` 的 Linux 自托管 runner，并安装 pVisor。下面先用不需要模型凭据的脚本验证整条流程，再把命令换成你的 Agent。所需宿主能力见[平台支持](../reference/platforms.md)。

```bash
cat > ci-agent.sh <<'SH'
#!/bin/sh
set -eu
printf 'CI task completed\n' > result.txt
SH
chmod +x ci-agent.sh
```

工作流执行成功后，下载产物，确认 `result.txt` 的改动和 Run 的结束状态。脚本返回非零时工作流失败，但 `always()` 步骤仍会收集已生成的记录；启动准备失败可能还没有 Bundle。

## 今天可以采用的最小流程

把标签 `pvisor` 分配给准备好的 runner，提交可执行的 `ci-agent.sh`，将工作流保存到 `.github/workflows/pvisor-task.yml`。从 Actions 手动触发一次，确认 Stage 产物可下载。

```yaml
name: Staged task
on: workflow_dispatch
jobs:
  run:
    runs-on: [self-hosted, linux, pvisor]
    steps:
      - uses: actions/checkout@v4
      - name: Run the task
        id: task
        shell: bash
        run: |
          set +e
          pvisor run --safe --overlaynet-deny-all \
            --stage "$RUNNER_TEMP/pvisor-stage-${GITHUB_RUN_ID}" \
            --timeout 5m --stdio capture -- ./ci-agent.sh
          code=$?
          echo "exit_code=$code" >> "$GITHUB_OUTPUT"
          exit "$code"
      - uses: actions/upload-artifact@v4
        if: always()
        with:
          name: pvisor-run-${{ github.run_id }}
          path: ${{ runner.temp }}/pvisor-stage-${{ github.run_id }}/
          include-hidden-files: true
          if-no-files-found: warn
```

替换为联网 Agent 时，按[网络策略](policies/network.md)配置具体目标，按[凭据与环境](policies/credentials.md)投影模型凭据。需要强制的选择性出口时使用准备好的 VM。

## 产物、失败与合入

- stage 放在 checkout 外，每次任务使用独立位置。保存 `run.json`、`run-bundle.json`、upper/preimage 与 ledger，不能只保存 diff 文本。
- `if: always()` 保留非零退出和超时后已经产生的记录；准备失败可能没有 Bundle，上传步骤警告不能被解释为成功。
- 示例不在 runner 中 apply，也不自动发布 PR。审查者确认来源、版本、原工作区基线与产物后，在相同基线的可信工作区中执行选择性 apply。
- 下载的 stage 含本机路径和来源假设，不是通用可移植 patch；跨机器合入需要保留/重建对应 workspace 和可读取记录，或在审查后导出普通 Git patch 走现有流程。
- Artifact 可能包含源码、提示与输出，应按仓库可见性和保留策略管理。完整失败分类见[退出码](../reference/exit-codes.md)。

## 加入凭据前先验证失败路径 {#failure-checks}

在已准备 runner 的临时工作区运行以下命令。每次使用新的 Stage 路径。第一个命令应返回 7，第二个应因超时返回 1，而非成功。两者均保留候选文件供审查。

```bash
set +e
pvisor run --safe --overlaynet-deny-all --stdio capture \
  --stage ../ci-failure-001 -- /bin/sh -c 'printf "candidate\n" > failed.txt; exit 7'
failure_code=$?
pvisor status --review --json ../ci-failure-001 > ../ci-failure-001.json
failure_review_code=$?
pvisor run --safe --overlaynet-deny-all --stdio capture --timeout 100ms \
  --stage ../ci-timeout-001 -- /bin/sh -c 'printf "candidate\n" > timed.txt; sleep 2'
timeout_code=$?
pvisor status --review --json ../ci-timeout-001 > ../ci-timeout-001.json
timeout_review_code=$?
printf 'failure=%s review=%s timeout=%s review=%s\n' \
  "$failure_code" "$failure_review_code" "$timeout_code" "$timeout_review_code"
```

预期 `failure=7 review=0 timeout=1 review=0`。检查两份 Bundle 后，再 drop 这些测试 Stage。实际采集的[失败](../../assets/examples/json/failed-run.json)与[超时](../../assets/examples/json/timeout-run.json)样例展示了对应结果。准备阶段失败则可能使 review 非零；上传已有记录，并保留原始运行退出码。

回归覆盖包括 `tests/documentation_json.rs`（实际非零退出/超时运行、候选暂存保留与已记录输出格式）。这些本地 Linux 检查验证工作流依赖的 CLI 行为，不是 GitHub Actions 托管 runner 的测量数据。
