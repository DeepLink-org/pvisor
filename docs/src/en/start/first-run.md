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

# Run your first Job {#local-workflow}

Let a command create two files, then keep only one. You do not need an Agent, model account, or VM image.

Before starting, [install pVisor and prepare your platform](installation.md#local-prerequisites): Linux needs FUSE and user namespaces; Apple Silicon macOS needs macFUSE 5.4.0+ with FSKit enabled. Run the commands below in the same terminal.

<a id="prepare-demo"></a>


## 1. Run a command {#run-and-stage}

Create a new directory and run a command:

```bash
mkdir pvisor-demo && cd pvisor-demo
pvisor spawn --safe --overlaynet-deny-all -- sh -c '
  echo "Hello from pVisor" > hello.txt
  echo "Temporary notes" > scratch.txt
'
```

The command has finished, but both files are still staged. Check that the original directory is unchanged:

```bash
test ! -e hello.txt && test ! -e scratch.txt && echo 'Workspace unchanged'
```

```text
Workspace unchanged
```

## 2. Review the changes {#review-changes}

Review the changes from this run:

```bash
pvisor review last --diff
```

You should see additions for `hello.txt` and `scratch.txt`, along with their text diffs. `last` selects the most recently registered Job for this workspace.

## 3. Keep the file you want {#apply-changes}

Accept only `hello.txt` and discard the remaining staged changes:

```bash
pvisor apply last --path hello.txt
pvisor drop last
cat hello.txt
test ! -e scratch.txt && echo 'scratch.txt: absent'
```

```text
Hello from pVisor
scratch.txt: absent
```

## What happened? {#what-happened}

`spawn --safe` runs the command in a copy-on-write workspace and records a Job. Both files stay staged. `review` lets you inspect them, `apply --path` writes the selected file into the original directory, and `drop` discards the rest.

`drop` does not undo accepted files or external API calls. Stop concurrent writers before applying; `apply` refuses to overwrite conflicting original files. See [review and apply](../guides/review-apply.md) for details.

!!! note "If startup fails"
    `--safe` refuses to start when the required isolation or staging is unavailable. Check [platform prerequisites](installation.md#local-prerequisites) and keep the flag; removing it makes ordinary Host Jobs write directly to the original workspace.

## Next steps {#next-steps}

### Use your own Agent

In your project, replace `sh -c '…'` with an installed Agent CLI:

```bash
pvisor spawn --safe -- claude
pvisor review last --diff
```

See [Agent integration](../guides/agents/index.md) for tools and model credentials.

- [Review and apply](../guides/review-apply.md): accept directories and handle conflicts.
- [VM tool execution](infrastructure.md): run tools in a Linux VM and collect feedback.
- [CLI reference](../reference/cli.md): look up commands and flags.
