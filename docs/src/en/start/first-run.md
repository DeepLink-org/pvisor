# Your first run

This example needs no Agent account or API key. A script plays a "fake agent": it edits source, deletes a file, writes an extra file, tries to read a sensitive path inside the project, and tries to reach the network. You then review the result and keep only the changes you want. Complete the [installation](installation.md) first, including the FUSE/macFUSE setup that host staging needs.

## 1. Create a demo project

```bash
mkdir -p pvisor-demo/project/src pvisor-demo/project/.ssh
cd pvisor-demo/project
printf 'print("v1")\n' > src/app.py
printf 'legacy\n' > src/legacy.txt
printf 'FAKE-KEY\n' > .ssh/id_rsa
git init -q && git add -A && git commit -qm init
```

`.ssh/id_rsa` is just a placeholder inside the project, used to show a sensitive path being denied inside the staged view. This example never touches your real `~/.ssh`.

## 2. Write a "fake agent"

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

## 3. Let it run unattended inside a boundary

```bash
pvisor run --safe --overlaynet-deny-all --stage ../stage-001 -- ../agent.sh
```

`--safe` requires filesystem and network isolation, and its preset denies sensitive paths such as `.ssh`, `.gnupg`, and private keys. It does not pick an executor and does not silently fall back to an unsandboxed host process. `--overlaynet-deny-all` installs a hard network boundary. Keep the stage outside the project and use a fresh directory for each run.

## 4. Review: the changes and the blocks are both recorded

This Job's storage *is* the stage directory, so address it by path:

```bash
pvisor status --review ../stage-001
```

**File access observations** list the paths and outcomes that reached OverlayFS: the write to `src/app.py`, the deletion of `src/legacy.txt`, the write to `scratch.txt`, and a denied read of `.ssh/id_rsa` (`denied=1`). **Network access observations** list the destinations and outcomes that reached OverlayNet: `example.com` was denied.

One distinction matters. On an ordinary host the selective proxy is cooperative—only requests sent through it are recorded, and an absent destination is not proof it was never reached. A hard network boundary comes from `--overlaynet-deny-all`, an offline container, or a VM; see [network control](../../zh/guides/network.md).

## 5. Keep only what you want

```bash
pvisor apply ../stage-001 --path src
pvisor drop ../stage-001
git status --short
```

`apply --path src` writes only the changes under `src` into the project; `drop` discards the remaining `scratch.txt`. If you changed the same file yourself during the run, `apply` refuses to overwrite it instead of merging silently.

## 6. Use your own command

```bash
pvisor run --safe --stage ../agent-stage-001 -- codex
pvisor status --review ../agent-stage-001
```

Installed Agent CLIs use the same entry point. When you use `--stage PATH`, pass that path or the printed Job ID (`run-*`) to later commands; `last` only resolves Runs in the default storage, so do not rely on it across projects.
