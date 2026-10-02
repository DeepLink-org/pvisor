---
status: todo
search:
  exclude: true
---

# 在 CI 中运行 Agent

!!! warning "规划中"
    本页尚无数据。下面是需求说明，欢迎认领。

## 要回答的问题

如何在 GitHub Actions 等流水线里让 Agent 无人值守地修问题，并把结果交给审查？

## 需求

- 指标：单次运行墙钟时间、资源占用、失败率、需要人工介入的次数
- 对照组：同一 Agent 在 CI 中直接运行
- 工作负载：用 Agent 修复失败的测试或执行例行重构
- 环境：GitHub Actions runner（Linux/macOS），固定 Agent 版本

## 验收标准

- 给出可复制的 workflow 示例，含 `--safe`、暂存路径与产物上传
- 明确 `apply` 在 CI 中的语义（谁审、何时合）
- 失败与超时路径有回归

## 关联

- 跟踪 issue：TODO
- 负责人：TODO
- 相关页面：guides/parallel-agents、reference/cli

## 今天可以采用的最小流程

下面是待环境验证的接入模板，不宣称完成 L2 自动免审。准备一个已安装相同 pVisor 版本、支持 FUSE/user namespace/Landlock 的 Linux runner，把标签 `pvisor` 分配给它；仓库中的 `ci-agent.sh` 必须可执行，先用离线脚本验证。默认 GitHub 托管 runner 的权限和 FUSE 条件不可直接假定。

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

网络模型 API 需要把 deny-all 替换为具体 allowlist 并显式交付凭据；Linux host 选择性代理仍是协作式，需要不可绕过的选择性出口时使用已准备好的 VM。不要为“能跑”而删除 safe 准入失败检查。

## 产物、失败与合入

- stage 放在 checkout 外，每次任务使用独立位置。保存 `run.json`、`run-bundle.json`、upper/preimage 与 ledger，不能只保存 diff 文本。
- `if: always()` 保留非零退出和超时后已经产生的记录；准备失败可能没有 Bundle，上传步骤警告不能被解释为成功。
- 示例不在 runner 中 apply，也不自动发布 PR。审查者确认来源、版本、原工作区基线与产物后，在相同基线的可信工作区中执行选择性 apply。
- 下载的 stage 含本机路径和来源假设，不是通用可移植 patch；跨机器合入需要保留/重建对应 workspace 和可读取记录，或在审查后导出普通 Git patch 走现有流程。
- Artifact 可能包含源码、提示与输出，应按仓库可见性和保留策略管理。完整失败分类见[退出码](../reference/exit-codes.md)。
