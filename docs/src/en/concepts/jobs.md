# Jobs and storage

A **Job** is persistent work in the CLI: a managed command, its execution evidence, and staged changes. `pvisor run` creates it; `status`, `kill`, `inspect`, `fork`, `apply`, and `drop` operate on it. The Job survives process exit so review can happen later.

Internally each Job corresponds to a Run record with ID `run-<uuid>` and results in `run-bundle.json`. See the [execution model](../design/execution-model.md) for Job/Run/Attempt relationships.

## Storage location

| Invocation | Storage |
| --- | --- |
| Default, including `--safe` and `--ask` | `~/.pvisor/runs/run-<uuid>/`; override root with `PVISOR_RUN_HOME` |
| Explicit `--stage PATH` | `PATH` itself |

If the default root falls inside the workspace lower layer, pVisor uses a system-temporary Run root so writable staging does not appear inside its own lower layer. See [project discovery](../reference/cli.md#run-项目发现) for layout.

## Selecting a Job

Lifecycle commands accept:

- `run-<uuid>`;
- a storage/stage directory, such as `../stage-001`;
- `run.json`, `upper`, or `merged` within a Job directory;
- a workspace path, selecting its latest Job;
- `last` or no selector, selecting the latest Job for the **current workspace**.

## How `last` resolves

`last` searches default storage only. Jobs created with `--stage PATH` live in that directory; pass their path or ID explicitly:

```bash
pvisor run --safe --stage ../stage-001 -- codex
pvisor status --review ../stage-001
pvisor apply ../stage-001 --path src
```

If no Job belongs to the current workspace, pVisor fails instead of choosing another project's Job. This prevents accidental apply across projects.

## Retention and cleanup

- Records and staged changes remain after execution until all changes are applied or dropped.
- Applying all remaining changes or dropping is terminal: disposable `upper`/`work` data is deleted, while compact Run/Overlay metadata, `apply-ledger.json`, and captured artifacts remain.
- Safe HOME and explicit `CODEX_HOME` use private stages discarded after execution; they are not included in the workspace Bundle.
