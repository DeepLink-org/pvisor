# Run agents in CI

Let an agent complete a CI task and hand its changes to a reviewer as an artifact. Upload the Stage and execution evidence after the task; the review step decides whether to apply the changes.

Prepare a Linux self-hosted runner that can run `pvisor run --safe`, with pVisor installed. First verify the workflow with a script that needs no model credentials, then replace it with your agent. See [Platform support](../reference/platforms.md) for host prerequisites.

```bash
cat > ci-agent.sh <<'SH'
#!/bin/sh
set -eu
printf 'CI task completed\n' > result.txt
SH
chmod +x ci-agent.sh
```

After a successful workflow, download the artifact and check the `result.txt` change and terminal Run status. A nonzero script exit fails the job, while the `always()` step still collects records that were produced. A setup failure may occur before a Bundle exists.

## Minimal flow you can use today

Assign the `pvisor` label to the prepared runner, commit executable `ci-agent.sh`, and save the workflow as `.github/workflows/pvisor-task.yml`. Trigger it manually from Actions and verify that the Stage artifact can be downloaded.

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

When replacing the script with a networked agent, configure destinations using [Network policies](policies/network.md) and project model credentials using [Credentials and environment](policies/credentials.md). Use a prepared VM when you need mandatory selective egress.

## Artifacts, failures, and merging

- Keep the stage outside checkout with a separate location per task. Retain `run.json`, `run-bundle.json`, the upper/preimages, and the ledger, not just text diffs.
- `if: always()` preserves the records already produced after a nonzero exit or timeout. Preparation may fail without a Bundle, and an upload-step warning must not be read as success.
- The example does not apply on the runner and does not publish a PR automatically. After a reviewer confirms provenance, version, the original workspace baseline, and the artifacts, apply selectively in a trusted workspace with the same baseline.
- A downloaded stage contains local paths and provenance assumptions and is not a portable patch. Cross-machine merging requires preserving or rebuilding the matching workspace and readable records, or exporting an ordinary Git patch after review and using your existing flow.
- Artifacts may contain source code, prompts, and output, so manage them according to your repository's visibility and retention policy. See [Exit codes](../reference/exit-codes.md) for the full failure classification.
