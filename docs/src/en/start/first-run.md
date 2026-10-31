---
description: Run a command, review its file changes, and keep only the files you choose.
---

<a id="your-first-run"></a>
<a id="1-prepare-a-project"></a>
<a id="2-write-the-fake-agent"></a>
<a id="demo-script"></a>
<a id="3-run-unattended-within-the-boundary"></a>
<a id="4-review-changes-and-blocked-accesses"></a>
<a id="5-apply-only-what-you-want"></a>
<a id="6-use-your-own-command"></a>
<a id="7-after-switching-to-a-real-task"></a>

# Run and review {#local-workflow}

Create two staged files, preview the changes, and keep only one. No Agent, model account, or VM image is needed.

**Before you start:** [install pVisor and prepare your platform](installation.md#local-prerequisites). Linux needs FUSE and user namespaces; Apple Silicon macOS needs macFUSE 5.4.0+ with FSKit enabled. Use the same terminal throughout.

<a id="prepare-demo"></a>

## 1. Run a command {#run-and-stage}

Create a fresh demo directory:

```bash
mkdir pvisor-demo
cd pvisor-demo
```

Run a command with file staging and outbound network access denied:

```bash
pvisor spawn --safe --overlaynet-deny-all -- sh -c '
  echo "Hello from pVisor" > hello.txt
  echo "Temporary notes" > scratch.txt
'
```

Both files are staged. Check that the original directory is unchanged:

```bash
test ! -e hello.txt && test ! -e scratch.txt && echo 'Workspace unchanged'
```

```text
Workspace unchanged
```

## 2. Review the changes {#review-changes}

```bash
pvisor review last --diff
```

You should see additions for `hello.txt` and `scratch.txt`, with their text diffs. `last` selects the most recently registered Job for this workspace.

Preview accepting only `hello.txt`, without writing to the workspace:

```bash
pvisor apply last --path hello.txt --dry-run --diff
```

Check that the preview includes `hello.txt`, not `scratch.txt`.

## 3. Keep one file {#apply-changes}

Apply your selection, discard the rest, and check the result:

```bash
pvisor apply last --path hello.txt  # Keep only hello.txt
pvisor drop last                   # Discard remaining staged changes
cat hello.txt
test ! -e scratch.txt && echo 'scratch.txt: absent'
```

The final two commands print:

```text
Hello from pVisor
scratch.txt: absent
```

## What happened? {#what-happened}

`spawn --safe` ran the command in a copy-on-write workspace and recorded a Job. `review` showed the staged edits; `apply --path` wrote only your selection to the original directory. `drop` discarded the rest.

Stop concurrent writers before applying; `apply` refuses to overwrite conflicting original files. File review and `drop` cannot undo external API calls, and `drop` leaves accepted files in place. See [review and apply](../guides/review-apply.md).

!!! note "If startup fails"
    `--safe` refuses to start when the required isolation or staging is unavailable. Check [platform prerequisites](installation.md#local-prerequisites) and keep the flag; removing it makes ordinary Host Jobs write directly to the original workspace.

## Next steps {#next-steps}

To use an installed Agent CLI in your project, replace the demo command:

```bash
pvisor spawn --safe -- claude
pvisor review last --diff  # Review before applying
```

- [Agent integration](../guides/agents/index.md): configure tools and model credentials.
- [Review and apply](../guides/review-apply.md): accept directories and handle conflicts.
- [VM tool execution](infrastructure.md): run tools in a Linux VM and collect feedback.
- [CLI reference](../reference/cli.md): look up commands and flags.
