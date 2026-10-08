# Concepts and boundaries

Judge three results separately for each task: whether the command completed, which net file changes it produced, and whether requested restrictions actually took effect. Exit codes, stages, and executor names describe different things and cannot substitute for each other.

## The Job is the review unit

A Job associates a managed command with its execution evidence and staged changes. After the command stops, you can still locate it and review the result; a failed command can leave useful candidate files.

In default storage, `last` searches by current workspace. An explicit `--stage PATH` stores the Job in that directory; use its path or the printed Job ID to avoid selecting the wrong result with custom storage or parallel projects. See [Jobs and storage](jobs.md) for identity and cleanup rules.

## The file view and publication are separate stages

With staging enabled, the Job reads workspace lower files combined with its own upper changes. Writes, deletions, and renames change that view first. Review shows the net effect; operation counts are not the final diff.

Apply compares selected changes against the baseline recorded at first modification before publishing to the original workspace. It refuses to overwrite targets changed by another writer. Stop other writers during publication: multi-file updates are not atomic against external editors. Drop only discards staged changes that have not been applied. See [staging and apply](staging.md) for the full contract and semantic cases.

Drop cannot undo explicitly shared direct-write host paths, applied batches, or remote API effects. Logical checkpoints save the upper and conflict preimages; they freeze neither every host lower file nor process memory or external service state.

## Check policy intent separately from installed controls

```text
Requested permissions → Policy narrowing and admission plan → Installed controls → Observed execution and outcome
```

User, workspace, session, and executor base policies narrow permissions layer by layer. An allow in one layer cannot override a deny in another. `Planned` in an admission plan means a mechanism is intended for installation, not that it has been enforced; executor observations at teardown report actual controls. See [policy model](policy-model.md) for policy composition.

Check file reads, file writes, network, subprocesses, credentials, and resources separately in the Run Bundle. Staging does not prove network isolation, and captured requests do not prove the absence of uncaptured connections. Observations cover only operations reaching their control points; `null` means unobserved, while zero means observed with no hits. See [capabilities and evidence](capabilities-and-evidence.md) for levels and guarantee scope.

## Records have distinct responsibilities

`run.json` stores Job identity, state, and local resources; the Run Bundle stores outcomes, control observations, and artifact summaries; an optional Event Journal links published facts and causal references. The Journal replaces neither the Bundle nor a global ordering of external side effects across Jobs.

These records support local review and diagnosis, not cryptographic remote attestation or hostile multi-tenancy guarantees. See the [execution model](../design/execution-model.md) for ownership of Operation, Attempt, and Session implementations.
