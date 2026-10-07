# Guides

Separate a task into execution, review, and publication: the command finishes in the selected environment, candidate files stay in the stage, and review precedes writing them back to the workspace. Prepare a host that supports staged Jobs using [first run](../start/first-run.md), and confirm the script or agent is installed.

## Pin inputs and access scope

Start from the task's project directory and use a fresh stage outside it for every Run. Pin the repository baseline and image digest when using an image. Choose host, container, or VM for the command's needs, and configure file access, network destinations, and credentials separately. Staging alone restricts neither reads nor networking.

For a fully offline script, start with these commands; the project's `task.sh` must already be executable:

```bash
pvisor run --safe --overlaynet-deny-all --stage ../task-stage-001 -- ./task.sh
pvisor status --review --diff ../task-stage-001
pvisor inspect ../task-stage-001 -- git status --short
```

When substituting a networked agent, choose an egress boundary and declare destinations using [network policies](policies/network.md), and deliver credentials using [credentials and environment](policies/credentials.md).

## Review before publication

After the Job stops, check outcome and exit code, installed controls and warnings, denied or failed accesses, and net file changes. `inspect` requires an OverlayFS workspace and uses host tools in a read-only view. Inspect content separately when text diffs are truncated or files are binary.

Select paths according to the task and apply them in batches with `apply --path`; unselected candidates remain staged. A full apply or drop cleans up disposable staging data. Stop other writers during apply, and follow [review and apply](review-apply.md) for conflicts and recovery after interruption. Drop discards only unapplied file changes, not applied batches or external service calls.

## Integrate the loop into automation

Give parallel tasks separate workspaces, stages, and explicit Job selectors; do not apply concurrently into one target tree. In CI, retain the stage and execution evidence, handle the run exit code separately from artifact upload, and let a review step decide what to apply. For evaluation, check execution results before reading candidates for scoring, then discard changes that need no publication.

To retry from file state, check workspace fork prerequisites in [next steps](../start/next-steps.md), then fork before applying or dropping all changes. The [CLI reference](../reference/cli.md) owns command syntax.
