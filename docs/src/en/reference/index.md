# Commands and scenarios

When integrating a task into a script, handle launch arguments, execution results, and file acceptance separately. Ordinary host execution writes directly to the workspace; use `--safe` or an explicit `--stage` to review files before accepting them.

## From try to file acceptance

```bash
pvisor spawn --safe --overlaynet-deny-all --stage ../stage-reference-001 -- /bin/sh -c 'printf proposal > report.txt'
pvisor status --review --json ../stage-reference-001 > ../review-reference-001.json
pvisor inspect ../stage-reference-001 -- /bin/cat report.txt
pvisor apply ../stage-reference-001 --path report.txt
```

After execution, `report.txt` stays in staging; `inspect` reads the proposal and `apply` accepts the selected file. Use `drop` to discard remaining staged changes you do not accept. Check execution and apply exit codes separately; workload exit 0 does not authorize automatic file acceptance. See [Exit codes and errors](exit-codes.md) for conflicts and failures. The [CLI reference](cli.md) defines argument syntax and storage defaults.

## Read records and use protocols

`status --json` provides a status overview; `status --review --json` provides the complete Run Bundle. Automation first checks format versions and required fields, then execution state, installed controls, and net file changes. Missing observations must not become zero or acceptance. See [Run Bundle](run-bundle.md) and [Machine-readable output](json-output.md) for fields and queries.

Shared image cache handles, authentication, and failures follow the [cache protocol](shared-image-cache.md), separately from Session staging and apply. Saving and restoring full machine state also requires the corresponding capability. A workspace checkpoint saves file proposals without continuing old process memory; check prerequisites against the [CLI execution-checkpoint contract](cli.md#full-vm-execution-checkpoints) before use.

## Check executable behavior

The [scenario appendix](cases.md) retains semantic claims and violation examples and links to the single Scrut check source in `tests/spec/<feature>/`. The repository entry `mise run cases` executes these checks. Execution success is not human approval; PASS does not authorize changes to human review ledgers.

## Scenario check preparation

The scenario appendix and VM checks share external product fixtures. Each check has independent CASE_ROOT and WS and an explicit SUBJECT_BIN. Preparation and assertions still require human review; maintainers have not yet decided the replacement digest-review policy.

Preparation and assertion source: [fixtures/doc.bash](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/fixtures/doc.bash).
