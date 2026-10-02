# Review and apply changes

Start a staged run from the project directory:

```bash
pvisor run --safe -- codex
pvisor status --review last
pvisor inspect last -- git status --short
```

Without an explicit path, `--safe` keeps workspace changes in the Job store. To choose a location, pass `--stage PATH` with a fresh directory outside the project for each Run.

`last` resolves the current workspace's most recent Job; after `--stage`, select that path instead of `last`. `status --review` shows evidence and changes, and `inspect` runs check commands in a read-only view. See [Jobs and storage](../concepts/jobs.md).

## Apply in batches

```bash
pvisor apply last --path src
pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
pvisor apply last --all
```

Each successful batch writes to the original workspace and consumes only the changes you selected. The rest stay staged for a later apply. Opaque directories and hard-link groups that depend on each other must be selected together.

Before applying, pVisor compares each target with its recorded original state, including recorded subpaths for recursive deletion and directory replacement. If another process changes a file inside the overwrite scope, apply reports a conflict instead of overwriting the newer content.

Stop other writers during apply: these checks cannot make a multi-file update atomic against an external editor. See the semantics behind each guarantee in [staging and apply](../concepts/staging.md).

`apply-ledger.json` persists batch progress so an interrupted apply can recover. Recovery accepts already-applied content only when it matches the expected result; any other target change is still an error. Inspect conflicts and keep external edits before retrying.

## Discard remaining changes

```bash
pvisor drop last
```

Drop removes only the staged changes that have not been applied. It cannot undo applied batches, network calls, or other external effects. To keep the current state and keep trying, see [checkpoints and forks](fork-checkpoint.md).

## Review order and expected results

Confirm the Job has stopped, then read in this order: outcome and exit code → actual file and network boundaries and warnings → denied or failed accesses → net file changes. Allow rules and captures do not replace mandatory-control evidence.

```bash
pvisor status --review --diff ../stage-001
pvisor inspect ../stage-001 -- git diff -- src
pvisor apply ../stage-001 --path src
pvisor status --review ../stage-001
```

After a successful selective apply, the host `src` shows the selected changes and the other unapplied paths remain in the review list. `changed_files` alone does not tell you whether a change is any good; text diffs have a total-byte and per-file limit, so inspect binary or truncated content separately.

## Handling conflicts

When a conflict occurs, keep the current workspace and stage; do not delete the preimage or hand-edit the ledger. Compare the host's current files, the staged files, and the original task to decide what to keep. To combine changes from both sides, resolve them explicitly in a fresh Job or a normal Git workflow and then retry; pVisor never performs an automatic three-way merge.

Record your final decision before dropping. Applied batches cannot be rolled back by drop, and a full apply or drop cleans the one-time staging data, so fork first when you need another attempt.
