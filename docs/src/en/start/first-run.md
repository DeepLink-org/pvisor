<a id="your-first-run"></a>

# Local workflow: run and review {#local-workflow}

Run a demo script with local tools and a workspace, then review and selectively apply its file changes. No VM image or model API is needed. Complete the [local workflow setup](installation.md#local-prerequisites) first.

<a id="1-prepare-a-project"></a>

## 1. Prepare the project and script {#prepare-demo}

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

<a id="demo-script"></a>

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

## 2. Run and stage changes {#run-and-stage}

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

## 3. Review candidate changes {#review-changes}

Select this Job by its stage path:

```bash
pvisor status --review ../stage-001
```

**Expected result:** the record includes a modification to `src/app.py`, deletion of `src/legacy.txt`, and addition of `scratch.txt`. Check the exit code, installed controls, and warnings before deciding which changes to accept; finishing execution does not apply changes.

<a id="5-apply-only-what-you-want"></a>

## 4. Apply only what you want {#apply-changes}

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

Replace the demo command with your own Agent CLI, using [Agent integration](../guides/agents/index.md) to prepare tools, model services and credentials. For a Linux image or an independent kernel, consult [Choose an executor](../guides/executors/index.md).
