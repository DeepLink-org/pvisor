# Job usage and storage {#job-storage}

A **Job** associates a managed command with execution evidence and staged changes.
CLI `stage` retains that state; `spawn` is temporary and removes its stage after exit.

```bash
pvisor stage --executor host --safe --stage ../stage-001 -- codex
pvisor status --review ../stage-001
```

`pvisor stage` creates a persistent Job; `status`, `kill`, `inspect`, `fork`, `apply`,
and `drop` operate on it directly, with no `job` subcommand. `ps -a` lists stopped
stages alongside containers and `rm` removes them. IDs retain the `run-*` name.

## Finding it again later {#selectors}

Lifecycle commands accept a selector: a Job ID, a stage directory, `run.json`, or a path within the view.

`last` searches registered Jobs by current workspace, including custom stage
storage. Names and unique ID prefixes also select instances; use an ID if a name
is ambiguous. Explicit paths remain useful:

```bash
pvisor status --review ../stage-001      # stage path
pvisor status --review run-20260102-abc  # or the Job ID
```

When no Job belongs to the current workspace, pVisor fails instead of substituting another project's Job.

## Storage location {#storage}

| Invocation | Storage |
| --- | --- |
| Default, including `--safe` | `~/.pvisor/runs/run-<uuid>/`; override the root with `PVISOR_RUN_HOME` |
| Explicit `--stage PATH` | `PATH` itself |

If the default root falls inside the workspace lower layer, pVisor uses a system-temporary Run root so writable staging does not appear inside its own lower layer. For the directory layout see [project discovery](../reference/cli.md#run-项目发现).

New CLI roots separate immutable `instance.json` identity from Attempt state in
`attempts/initial/`. Each Attempt owns its Run record, Bundle, completion receipt,
lease, `upper/`, `work/`, `merged/`, `preimages/` and logical checkpoints. Native
VM lifecycle metadata stays at the instance root and selects the active Attempt;
resume creates another directory under `attempts/`. Old flat layouts stay readable
without moving existing files.

## Retention and cleanup {#cleanup}

- `spawn` removes its instance storage after confirmed exit, including failure or cancellation. Unknown completion retains recovery evidence and reports the refusal.
- `stage` retains its instance after exit until explicit `pvisor rm ID`. `drop` discards pending candidates but does not remove the instance.
- Applying all remaining changes or dropping is terminal: disposable `upper`/`work` data is deleted, while compact Run/Overlay metadata, `apply-ledger.json`, and captured artifacts remain.
- Safe HOME and explicit `CODEX_HOME` use private stages discarded after execution; they are not included in the workspace Bundle.
- Filesystem `fork` creates independent native-CoW inodes, so deleting its parent does not invalidate the child. Execution-state branches retain separate snapshot references and can block source removal until those branches are removed.
