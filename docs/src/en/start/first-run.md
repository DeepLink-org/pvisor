# Your first run

This demo needs no agent account or API key. A script plays a "fake agent": it edits source, deletes a file, adds another file, attempts to read a sensitive project path and reach the internet. You then review the result and apply only the changes you want. Complete [installation](installation.md) first, including the FUSE/macFUSE setup that staged host execution needs.

## 1. Prepare a project

```bash
mkdir -p pvisor-demo/project/src pvisor-demo/project/.ssh
cd pvisor-demo/project
printf 'print("v1")\n' > src/app.py
printf 'legacy\n' > src/legacy.txt
printf 'FAKE-KEY\n' > .ssh/id_rsa
git init -q && git add -A && git commit -qm init
```

`.ssh/id_rsa` is a fake project fixture. This demo does not read or write your real `~/.ssh`.

## 2. Write the fake agent

```bash
cat > ../agent.sh <<'SH'
#!/usr/bin/env bash
set -u
printf 'print("patched")\n' >> src/app.py     # edit source
rm -f src/legacy.txt                          # delete a file
printf 'scratch\n' > scratch.txt              # extra change at the workspace root
cat .ssh/id_rsa >/dev/null 2>&1 || echo 'read .ssh/id_rsa: denied'
curl -sS --max-time 5 https://example.com >/dev/null 2>&1 || echo 'reach example.com: blocked'
SH
chmod +x ../agent.sh
```

## 3. Run unattended within the boundary

```bash
pvisor spawn --safe --overlaynet-deny-all --stage ../stage-001 -- ../agent.sh
```

`--safe` requires filesystem and network isolation and rejects `.ssh`, `.gnupg`, and common private-key paths through its preset. It does not choose an executor or silently fall back to an unisolated host process. `--overlaynet-deny-all` independently installs a mandatory network boundary. Keep the stage outside the project and use a fresh directory for each run.

## 4. Review changes and blocked accesses

This Job is stored in the stage directory; select it by path:

```bash
pvisor status --review ../stage-001
```

**File access observations** list operations reaching OverlayFS: writes to `src/app.py` and `scratch.txt`, deletion of `src/legacy.txt`, and denial of the `.ssh/id_rsa` read (`denied=1`). **Network access observations** list destinations reaching OverlayNet, including the rejected `example.com` request.

Selective proxies on ordinary host execution depend on client cooperation and record only requests that pass through them. A destination missing from the record may still have been accessed. Mandatory boundaries come from deny-all, container offline mode, or VM networking; see [network control](../guides/policies/network.md).

## 5. Apply only what you want

```bash
pvisor apply ../stage-001 --path src
pvisor drop ../stage-001
git status --short
```

`apply --path src` writes only changes under `src` to the project. `drop` discards the remaining `scratch.txt`. If you changed the same file during the run, `apply` refuses to overwrite it rather than silently merging.

## 6. Use your own command

```bash
pvisor spawn --safe --stage ../agent-stage-001 -- codex
pvisor status --review ../agent-stage-001
```

Installed Agent CLIs use the same entry point. With `--stage PATH`, pass that path or the printed Job ID (`run-*`) to later commands. `last` searches default storage only; do not rely on it when running projects in parallel.

## 7. After switching to a real task

Before running a real task, limit its scope, retain the current repository baseline and confirm the agent is installed in the selected execution environment. Configure its model service and credentials; pVisor does not select the agent's provider. For an installed Codex with an explicitly delivered OpenAI key, set `OPENAI_API_KEY` in the host environment first:

```bash
pvisor spawn --safe --pass-env OPENAI_API_KEY --stage ../agent-task-001 -- codex
pvisor status --review ../agent-task-001
pvisor inspect ../agent-task-001 -- git --no-pager diff -- src
```

`--safe` matches network presets by the direct executable filename. Adding `--overlaynet-allow` replaces the preset list, so list model APIs alongside dependency downloads or other model services. HOME writes are discarded after exit; do not depend on this run to persist login state. See [Agent integration](../guides/agents/index.md) for other agents' credentials and prerequisites.

After the Job stops, check its exit code, installed controls, warnings and changes; a nonzero exit can still leave candidates. Choose which files to keep, then follow [review and apply](../guides/review-apply.md) to handle application and conflicts. To keep the file proposal and try another approach, check the [workspace fork prerequisites](../guides/fork-checkpoint.md) before cleaning up the stage.

Two constraints apply before expanding the task scope. Selective proxies on the Linux host remain cooperative, so prepare a VM with the agent installed when you need mandatory selective egress ([credentials and environment](../guides/policies/credentials.md)). When adding concurrency, give each task a separate worktree and stage, select results by explicit paths, review each separately, and never apply concurrently into the same target tree; the [parallel workspace workflow](../guides/parallel-agents.md#batch) verifies records and merging with two offline tasks before you substitute real agents.
