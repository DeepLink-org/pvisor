---
status: todo
search:
  exclude: true
---

# Run agents in CI

!!! warning "Planned"
    No results are available yet. This page records the requirements; contributions are welcome.

## Question

How can a GitHub Actions-style pipeline run unattended agents and submit results for review?

## Requirements

- Metrics: wall time, resources, failure rate, interventions.
- Control: direct agent execution in CI.
- Workload: repair failing tests or perform routine refactoring.
- Environment: Linux/macOS runners, pinned agent versions.

## Acceptance criteria

- Copyable workflow with `--safe` mode, stage paths, and artifact upload.
- Explain who reviews and when apply occurs.
- Regress failure/timeout paths.

## Tracking

- Tracking issue: TODO (no issue has been assigned).
- Owner: TODO
- Related: [Parallel agents](parallel-agents.md), [CLI](../reference/cli.md)

## Minimal workflow available today

This is an integration template awaiting environment validation, not L2 automatic exemptions. Provision a Linux runner with matching pVisor, FUSE, user namespaces, and Landlock; give it the pvisor label. Make ci-agent.sh executable and validate offline first. Do not assume default hosted runners meet these requirements.

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

For model APIs replace deny-all with explicit allowlists and grant credentials. Selective Linux host proxies remain cooperative; provision a VM for mandatory selective egress. Do not remove safe admission checks merely to make execution start.

## Artifacts, failures, and merging

- Keep stages outside checkout with separate paths per task. Retain run.json, Bundle, upper/preimages, and ledger, not just text diffs.
- Always-upload preserves available failure/timeout records. Preparation may fail without a Bundle; upload warnings do not imply success.
- The template does not apply on the runner or publish PRs. A reviewer verifies provenance, version, and workspace baseline before applying in a trusted matching workspace.
- Stages include local paths/provenance assumptions and are not portable patches. Restore matching workspace/readable records across hosts or export a reviewed ordinary Git patch through your existing flow.
- Artifacts may contain code, prompts, and output. Use repository visibility/retention rules. See [exit codes](../reference/exit-codes.md).
