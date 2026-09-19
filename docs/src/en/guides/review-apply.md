# Review and apply changes

Start a staged Run from the project directory. Keep the stage outside the project and choose a new directory for each Run:

```bash
pvisor run --stage ../stage-001 -- codex
pvisor review last
pvisor inspect last -- git status --short
```

Without an OverlayFS option such as `--stage`, a host Run can modify the real project directly. `review` reports recorded evidence and the staged changes; `inspect` runs a command against a read-only view.

## Apply a selected batch

```bash
pvisor apply last --path src
pvisor apply last --include 'tests/**' --exclude 'tests/generated/**'
pvisor apply last --all
```

Each successful batch writes to the original workspace and consumes only its selected changes. Remaining changes stay staged and can be applied later. Opaque directories and hard-link groups must be selected together when their dependencies require it.

Before applying, pVisor compares the target with the recorded preimages. A recursive deletion or directory replacement also checks recorded descendants. If another process changed a covered file, apply fails rather than overwriting that change. Stop other writers during apply: these checks do not make a multi-file update atomic with respect to an external editor.

A durable `apply-ledger.json` tracks each batch across recovery. Recovery accepts already-applied content only when it matches the intended result; conflicting target content remains an error. Review the conflict and preserve the external edit before retrying.

## Keep a checkpoint or discard the remainder

Before fully applying or dropping the stage:

```bash
pvisor checkpoint last --name before-experiment
pvisor fork last --checkpoint before-experiment -- codex
```

A CLI checkpoint requires a stopped Run. It captures the filesystem upper layer and lineage, not process memory or an immutable snapshot of all underlying host files.

```bash
pvisor drop last
```

Dropping discards the remaining staged changes. It cannot undo batches already applied, network calls, or other external effects. For a repeatable workspace across commands, see [`env` in the CLI reference](../reference/cli.md).
