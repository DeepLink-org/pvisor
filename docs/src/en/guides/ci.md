---
status: todo
search:
  exclude: true
---

# Run agents in CI

!!! warning "Planned"
    No results are available yet. This page records the requirements; contributions are welcome.

## Question

How can a pipeline such as GitHub Actions run unattended agents to fix problems and hand the results to review?

## Requirements

- Metrics: wall clock time for one run, resource use, failure rate, and the number of human interventions.
- Control: the same agent running directly in CI.
- Workload: use the agent to repair failing tests or perform routine refactoring.
- Environment: GitHub Actions runners (Linux/macOS), pinned agent versions.

## Acceptance criteria

- A copyable workflow example with `--safe`, staging paths, and artifact upload.
- Explicit `apply` semantics in CI (who reviews, when it merges).
- Regression coverage for failure and timeout paths.

## Tracking

- Tracking issue: TODO
- Owner: TODO
- Related pages: [Parallel agents](parallel-agents.md), [CLI](../reference/cli.md)

## Minimal flow you can use today

This integration template awaits environment validation and does not claim L2 automatic exemption from review. Provision a Linux runner with the same pVisor version installed and support for FUSE, user namespaces, and Landlock, then assign it the `pvisor` label; the repository's `ci-agent.sh` must be executable, and you should validate it with an offline script first. Do not assume the permissions and FUSE conditions of default GitHub-hosted runners.

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

For network model APIs, replace deny-all with a concrete allowlist and deliver credentials explicitly. Selective Linux host proxies are still cooperative; when you need non-bypassable selective egress, use a prepared VM. Never remove safe admission failure checks just to make it run.

## Artifacts, failures, and merging

- Keep the stage outside checkout with a separate location per task. Retain `run.json`, `run-bundle.json`, the upper/preimages, and the ledger, not just text diffs.
- `if: always()` preserves the records already produced after a nonzero exit or timeout. Preparation may fail without a Bundle, and an upload-step warning must not be read as success.
- The example does not apply on the runner and does not publish a PR automatically. After a reviewer confirms provenance, version, the original workspace baseline, and the artifacts, apply selectively in a trusted workspace with the same baseline.
- A downloaded stage contains local paths and provenance assumptions and is not a portable patch. Cross-machine merging requires preserving or rebuilding the matching workspace and readable records, or exporting an ordinary Git patch after review and using your existing flow.
- Artifacts may contain source code, prompts, and output, so manage them according to your repository's visibility and retention policy. See [Exit codes](../reference/exit-codes.md) for the full failure classification.
