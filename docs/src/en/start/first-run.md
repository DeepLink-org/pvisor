<a id="your-first-run"></a>

# Local workflow: run and review {#local-workflow}

Run a demo script that changes files, then review and selectively apply the result. No agent account or API key is needed. Complete [installation](installation.md) first, including the FUSE/macFUSE setup required for staged host execution.

## 1. Prepare a project

```bash
mkdir -p pvisor-demo/project/src
cd pvisor-demo/project
printf 'print("v1")\n' > src/app.py
printf 'legacy\n' > src/legacy.txt
git init -q
git add src
git -c user.name='pVisor Demo' -c user.email='demo@example.invalid' -c commit.gpgsign=false commit -qm init
```

Use a new demo directory. The Git identity applies only to this baseline commit and does not change your global configuration.

<a id="2-write-the-fake-agent"></a>

## 2. Write the demo script {#demo-script}

The script modifies `src/app.py`, deletes `src/legacy.txt`, and creates `scratch.txt`, which you will leave out of the accepted changes:

```bash
cat > ../agent.sh <<'SH'
#!/usr/bin/env bash
set -eu
printf 'print("patched")\n' >> src/app.py
rm src/legacy.txt
printf 'scratch\n' > scratch.txt
SH
chmod +x ../agent.sh
```

<a id="3-run-unattended-within-the-boundary"></a>

## 3. Run and stage changes {#run-and-stage}

```bash
pvisor spawn --safe --overlaynet-deny-all --stage ../stage-001 -- ../agent.sh
```

`--safe` requires file and network isolation and refuses to start if it cannot enforce them. `--overlaynet-deny-all` enforces a boundary blocking external network access. `--stage` sets the candidate-change storage location; keep it outside the project and use a fresh directory for each run.

**Expected result:** the script exits successfully, with all three changes staged and the original project unchanged. Check from your current terminal:

```bash
cat src/app.py
cat src/legacy.txt
test ! -e scratch.txt && echo 'scratch.txt: absent'
```

Expected output:

```text
print("v1")
legacy
scratch.txt: absent
```

<a id="4-review-changes-and-blocked-accesses"></a>

## 4. Review candidate changes {#review-changes}

Select this Job by its stage path:

```bash
pvisor status --review ../stage-001
```

**Expected result:** the record includes a modification to `src/app.py`, deletion of `src/legacy.txt`, and addition of `scratch.txt`. Check the exit code, installed controls, and warnings before deciding which changes to accept; finishing execution does not apply changes.

To test access restrictions further, configure [file policies](../guides/policies/files.md) and [network control](../guides/policies/network.md). An ordinary host's selective proxy only records requests routed through it; a missing destination does not prove it was never accessed. This demo uses an enforced deny-all boundary.

## 5. Apply only what you want

```bash
pvisor apply ../stage-001 --path src
pvisor drop ../stage-001
git diff -- src
```

**Expected result:**

| File | Before applying | After applying and dropping remaining changes |
| --- | --- | --- |
| `src/app.py` | Contains only `print("v1")` | Adds a line with `print("patched")` |
| `src/legacy.txt` | Still exists | Deleted |
| `scratch.txt` | Absent from the project; exists only in the stage | Still absent from the project; candidate discarded |

`apply --path src` accepts only changes under `src`; `drop` discards remaining candidates without undoing accepted changes. `git diff -- src` should show the added source line and deleted file. If you edited the same file during execution, `apply` refuses to overwrite it; see [review and apply](../guides/review-apply.md) for conflict handling.

<a id="6-use-your-own-command"></a>
<a id="7-after-switching-to-a-real-task"></a>

## Next steps {#next-steps}

For a real agent, first confirm it is installed in the selected environment. Configure its model service and credentials using [Agent integration](../guides/agents/index.md), then replace the demo command with the agent CLI. With a custom `--stage`, pass that path or the printed Job ID to later commands; `last` only searches default storage.

For a Linux image or independent kernel, [choose an executor](../guides/executors/index.md). To retry from existing file state or explore in parallel, use [checkpoints and forks](../guides/fork-checkpoint.md) and the [parallel workspace workflow](../guides/parallel-agents.md#batch).
