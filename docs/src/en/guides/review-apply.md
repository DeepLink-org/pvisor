# Review and apply changes

Start a staged run from the project:

```bash
pvisor run --safe -- codex
pvisor status --review last
pvisor inspect last -- git status --short
```

`--safe` preserves changes without an explicit path and last selects the current workspace's latest Job. With an explicit `--stage PATH`, use a fresh directory outside the project and select it by path rather than last; see [Jobs](../concepts/jobs.md). Review shows evidence and changes; inspect runs read-only checks.

## Apply in batches

```bash
pvisor apply last --path src
pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
pvisor apply last --all
```

Each successful batch writes to the original workspace and consumes selected changes only. Remaining changes can be applied later. Dependent opaque directories/hard-link groups must be selected together.

Before writes, pVisor compares targets with recorded preimages, including descendants for recursive deletion/directory replacement. Conflicting external edits cause refusal. Stop other writers during apply: multi-file updates are not atomic against external editors. See [staging specifications](../concepts/staging.md).

Apply ledger persists batch progress for forward recovery after interruption. Recovery accepts already-applied content only when it matches the expected result; other changes remain errors. Inspect conflicts and preserve external edits before retrying.

## Discard remaining changes

```bash
pvisor drop last
```

Drop removes unapplied staging only. It cannot undo applied batches, network calls, or other effects. [Fork a checkpoint](fork-checkpoint.md) before dropping if you want another attempt from this state.

## Review order and expected results

Confirm the Job stopped. Read outcome/exit code, installed file/network boundaries and warnings, denied/failed accesses, then net changes. Grants and captures do not replace mandatory-control evidence.

```bash
pvisor status --review --diff ../stage-001
pvisor inspect ../stage-001 -- git diff -- src
pvisor apply ../stage-001 --path src
pvisor status --review ../stage-001
```

After selective apply, host src contains selected changes and other paths remain in review. Change counts do not establish quality. Text diffs have total/per-file limits; inspect binary or truncated content separately.

## Handling conflicts

Keep both workspace and stage. Do not delete preimages or edit the ledger. Compare current host content, staged content, and task requirements. Resolve combined changes explicitly in a fresh Job or normal Git workflow before retrying; pVisor does not perform automatic three-way merges.

Record the decision before drop. Applied batches cannot be rolled back with drop; full apply/drop cleans temporary staging, so fork first if needed.
