# Jobs and storage

A **Job** is a persistent unit of work in pVisor: one managed command, its execution evidence, and staged changes.

```bash
pvisor run --safe --stage ../stage-001 -- codex
pvisor status --review ../stage-001
```

`pvisor run` creates a Job; `status`, `kill`, `inspect`, `fork`, `apply`, and `drop` operate on it directly, with no `job` subcommand. On disk it keeps the `run-*` and Run Bundle names.

## Finding it again later

Lifecycle commands accept a selector: a Job ID, a stage directory, `run.json`, or a path within the view.

`last` is a convenience that searches **default storage** only, by current workspace. With `--stage PATH` the Job lives in the stage directory, not default storage, so pass the path or ID explicitly:

```bash
pvisor status --review ../stage-001      # 推荐：用暂存路径
pvisor status --review run-20260102-abc  # 或者用 Job ID
```

When no Job belongs to the current workspace, pVisor fails instead of substituting another project's Job.

## Storage location

| Invocation | Storage |
| --- | --- |
| Default, including `--safe` and `--ask` | `~/.pvisor/runs/run-<uuid>/`; override the root with `PVISOR_RUN_HOME` |
| Explicit `--stage PATH` | `PATH` itself |

If the default root falls inside the workspace lower layer, pVisor uses a system-temporary Run root so writable staging does not appear inside its own lower layer. For the directory layout see [project discovery](../reference/cli.md#run-项目发现).

## Retention and cleanup

- Records and staged changes remain after execution until all changes are applied or dropped.
- Applying all remaining changes or dropping is terminal: disposable `upper`/`work` data is deleted, while compact Run/Overlay metadata, `apply-ledger.json`, and captured artifacts remain.
- Safe HOME and explicit `CODEX_HOME` use private stages discarded after execution; they are not included in the workspace Bundle.
