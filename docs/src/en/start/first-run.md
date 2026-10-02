# Your first run

This demo needs no agent account or API key. A script plays the agent: it edits source, deletes a file, adds another file, attempts to read a sensitive project path and access the internet. You then review and apply only the desired changes. Complete [installation](installation.md), including FUSE/macFUSE for staged host execution, first.

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
printf 'print("patched")\n' >> src/app.py     # 修改源码
rm -f src/legacy.txt                          # 删除文件
printf 'scratch\n' > scratch.txt              # 工作区根目录的额外改动
cat .ssh/id_rsa >/dev/null 2>&1 || echo 'read .ssh/id_rsa: denied'
curl -sS --max-time 5 https://example.com >/dev/null 2>&1 || echo 'reach example.com: blocked'
SH
chmod +x ../agent.sh
```

## 3. Run unattended within the boundary

```bash
pvisor run --safe --overlaynet-deny-all --stage ../stage-001 -- ../agent.sh
```

`--safe` requires filesystem and network isolation and rejects `.ssh`, `.gnupg`, and common private-key paths through its preset. It does not choose an executor or silently fall back to an unisolated host process. `--overlaynet-deny-all` independently installs a mandatory network boundary. Keep the stage outside the project and use a fresh directory for each run.

## 4. Review changes and blocked accesses

This Job is stored in the stage directory; select it by path:

```bash
pvisor status --review ../stage-001
```

**File access observations** list operations reaching OverlayFS: writes to `src/app.py` and `scratch.txt`, deletion of `src/legacy.txt`, and denial of the `.ssh/id_rsa` read (`denied=1`). **Network access observations** list destinations reaching OverlayNet, including the rejected `example.com` request.

Selective proxies on ordinary host execution are cooperative: only requests through the proxy are recorded. An absent destination does not prove it was never accessed. Mandatory boundaries come from deny-all, container offline mode, or VM networking; see [network control](../guides/policies/network.md).

## 5. Apply only what you want

```bash
pvisor apply ../stage-001 --path src
pvisor drop ../stage-001
git status --short
```

`apply --path src` writes only changes under `src` to the project. `drop` discards the remaining `scratch.txt`. If you changed the same file during the run, `apply` refuses to overwrite it rather than silently merging.

## 6. Use your own command

```bash
pvisor run --safe --stage ../agent-stage-001 -- codex
pvisor status --review ../agent-stage-001
```

Installed Agent CLIs use the same entry point. With `--stage PATH`, pass that path or the printed Job ID (`run-*`) to later commands. `last` searches default storage only; do not rely on it across projects.
