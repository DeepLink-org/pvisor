# Commands and scenarios

When integrating a task into a script, handle launch arguments, execution results, and file acceptance separately. Ordinary host execution writes directly to the workspace; use `--safe`, `--ask`, or an explicit `--stage` to review files before accepting them.

## From execution to file acceptance

```bash
pvisor run --safe --overlaynet-deny-all --stage ../stage-reference-001 -- /bin/sh -c 'printf proposal > report.txt'
pvisor status --review --json ../stage-reference-001 > ../review-reference-001.json
pvisor inspect ../stage-reference-001 -- /bin/cat report.txt
pvisor apply ../stage-reference-001 --path report.txt
```

After execution, `report.txt` stays in staging; `inspect` reads the proposal and `apply` accepts the selected file. Use `drop` to discard remaining staged changes you do not accept. Check execution and apply exit codes separately; workload exit 0 does not authorize automatic file acceptance. See [Exit codes and errors](exit-codes.md) for conflicts and failures. The [CLI reference](cli.md) defines argument syntax and storage defaults.

## Read records and use protocols

`status --json` provides a status overview; `status --review --json` provides the complete Run Bundle. Automation first checks format versions and required fields, then execution state, installed controls, and net file changes. Missing observations must not become zero or acceptance. See [Run Bundle](run-bundle.md) and [Machine-readable output](json-output.md) for fields and queries.

Shared image cache handles, authentication, and failures follow the [cache protocol](shared-image-cache.md), separately from Job staging and apply. Saving and restoring full machine state also requires the corresponding capability. A workspace checkpoint saves file proposals without continuing old process memory; check prerequisites against the [CLI execution-checkpoint contract](cli.md#full-vm-execution-checkpoints) before use.

## Check executable behavior

The [scenario appendix](cases.md) retains product commands, semantic claims, and assertions, and serves as the DOC semspec source. Runner fixtures in its code blocks cannot be run as ordinary shell commands; the repository entry `just cases` executes the checks. Execution success and human approval are recorded separately; PASS does not authorize changes to human review ledgers.
