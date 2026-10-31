---
description: Run an offline command, inspect staged files, selectively apply changes, and discard the rest, step by step.
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

# Get started with either mode {#local-workflow}

Complete [installation](installation.md), then choose a path. Prepare only the prerequisites for your selected mode.

- [Docker style sandbox](#docker-style): pull an image, start a separate VM, execute commands, then stop and remove the container.
- [Agent style stagebox](#agent-style): create two files with an offline command, review and accept one, then discard the remaining change.

## Docker style sandbox {#docker-style}

Use your local terminal. Linux x86_64 needs read/write access to `/dev/kvm`; Apple Silicon macOS needs a release package with the Hypervisor entitlement. Complete [VM platform setup](installation.md#docker-prerequisites). No Docker Daemon or model account is needed, and this example mounts no host directory.

### 1. Prepare an image {#docker-image}

```bash
pvisor pull alpine:3.20
```

Wait for the download to complete. First use needs registry access and enough cache space; later runs can reuse the cache. Resolve download failures by checking connectivity and disk space before starting a container.

### 2. Execute one command {#docker-run}

```bash
pvisor run --rm --network none alpine:3.20 echo hello
```

Expect output containing `hello`. `--network none` disables guest networking; image download happens on the host. After exit, `--rm` removes this container while retaining the image cache.

### 3. Keep it running and execute tools {#docker-exec}

Choose an unused name. If `pvisor-demo` exists, replace it throughout the following commands:

```bash
pvisor run -d --name pvisor-demo --network none alpine:3.20 sleep 600
pvisor ps
pvisor exec pvisor-demo sh -c 'echo "inside the VM" > /tmp/result.txt'
pvisor exec pvisor-demo cat /tmp/result.txt
```

`ps` should list a running `pvisor-demo`; the last command should print `inside the VM`. Both `exec` calls use the same container. The file lives in the VM's `/tmp`, outside your current host directory. `sleep 600` keeps it running for up to ten minutes; finish the remaining steps before it expires.

### 4. Stop and clean up {#docker-cleanup}

```bash
pvisor stop pvisor-demo
pvisor rm pvisor-demo
pvisor ps -a
```

The final list should no longer contain `pvisor-demo`. If ten minutes have elapsed and it has stopped, skip `stop` and run `rm` directly. Removal clears the container's writable layer, including `/tmp/result.txt`, while keeping cached images for reuse.

| Problem | Next action |
| --- | --- |
| KVM permissions or virtualization unavailable | Check [VM prerequisites](installation.md#docker-prerequisites); virtualized servers also need nested virtualization. |
| Image architecture mismatch | Use a native-architecture image; CPU emulation is not provided. |
| Name already exists | Inspect `pvisor ps -a` and choose another name; remove only containers you know belong to this demo. |
| `exec` cannot find a running instance | Check status and name with `pvisor ps -a`, including whether the sleep deadline elapsed. |

This mode manages VMs through container commands. See the [container reference](../reference/cli.md#container-commands) for flags and volume persistence. This demo configures no volumes and needs no Job review/apply/drop.

## Agent style stagebox {#agent-style}

Use your local shell to create two candidate files and hand only the greeting back to the original directory. No model account, VM image, or running Daemon is needed. Run the steps in order in the same terminal and directory.

### Before you start {#before-you-start}

Complete [installation and platform setup](installation.md). Linux x86_64 needs FUSE and user namespaces; Apple Silicon macOS needs macFUSE 5.4.0+ with FSKit enabled.

If already installed, activate the virtual environment used for installation and check the CLI:

```bash
source ~/.venvs/pvisor/bin/activate
pvisor --version
```

You should see a version number. Skip `source` for Homebrew or native packages. If the command is missing, follow the [installation checks](installation.md#verify-installation) before continuing.

<a id="prepare-demo"></a>

### 1. Create a demo directory {#prepare-workspace}

Open a terminal where you want to keep the demo files and create an empty directory:

```bash
mkdir pvisor-demo
cd pvisor-demo
pwd
```

Run the remaining commands here. If the directory exists, choose a new name so files from an earlier run do not affect the result. You do not need to clone pVisor or initialize Git.

### 2. Create files in stagebox mode {#run-and-stage}

Run this command:

```bash
pvisor spawn --safe --overlaynet-deny-all -- sh -c '
  echo "Hello from pVisor" > hello.txt
  echo "Temporary notes" > scratch.txt
'
```

Wait for the command to finish and return to your terminal prompt. `spawn` starts and records a Job; `--safe` requests isolation and workspace staging; `--overlaynet-deny-all` disables networking for this offline task; everything after `--` is the program and its arguments.

The command writes to files without printing their contents. Both files should remain staged. Check that neither appeared in the original directory:

```bash
test ! -e hello.txt && test ! -e scratch.txt && echo 'Workspace unchanged'
```

Expected output:

```text
Workspace unchanged
```

If this line is absent, stop before applying anything. Check that the launch command included `--safe`, that the Job started successfully, and that you are in the right directory. For startup failures, see [troubleshooting](#troubleshooting) below.

### 3. Review the staged changes {#review-changes}

Check the Job status and file diff:

```bash
pvisor status last
pvisor review last --diff
```

`status` should show a stopped task. `review` should list two additions, `hello.txt` and `scratch.txt`, containing `Hello from pVisor` and `Temporary notes`. Identities, paths, and timestamps vary between runs.

`last` selects the most recently registered Job for this workspace. Do not start another Job here until you finish the tutorial, because `last` would then select the new task. Use explicit [Job identities](../reference/jobs.md) for parallel work.

You can also read a file in the task's read-only view:

```bash
pvisor inspect last -- cat hello.txt
```

Expect `Hello from pVisor`. You can read the file inside the Job. In the original directory, `cat hello.txt` should still report a missing file.

Preview the selection before writing to the original directory:

```bash
pvisor apply last --path hello.txt --dry-run --diff
```

The preview should contain only `hello.txt` and leave both files staged.

### 4. Apply one file {#apply-changes}

Keep the greeting and leave the temporary notes out. Accept only `hello.txt`:

```bash
pvisor apply last --path hello.txt
cat hello.txt
test ! -e scratch.txt && echo 'scratch.txt: absent'
```

The output should include:

```text
Hello from pVisor
scratch.txt: absent
```

`hello.txt` is now in the original directory; `scratch.txt` remains an unaccepted candidate. Stop other processes that might write the same files before applying. If an original file conflicts, `apply` refuses to overwrite it. Keep the stage and follow [conflict handling](../guides/review-apply.md) before deciding what to do.

### 5. Discard the remaining change {#discard-changes}

Once you are sure you do not need the temporary notes, discard the remaining candidate:

```bash
pvisor drop last
cat hello.txt
test ! -e scratch.txt && echo 'scratch.txt: absent'
```

Expect the same two lines again. `drop` leaves the accepted `hello.txt` in place.

### What happened? {#what-happened}

| Operation | Files in the Job | Files in the original directory |
| --- | --- | --- |
| `spawn --safe` completes | Both files are candidates | Neither file exists |
| `apply --path hello.txt` completes | Temporary notes still await a decision | Only `hello.txt` |
| `drop` completes | Remaining candidates discarded | Still only `hello.txt` |

File staging controls acceptance of file changes. `drop` does not undo accepted files or external API calls. Network and read restrictions depend on the executor and installed controls; see [security boundaries](../security/index.md).

### Troubleshooting {#troubleshooting}

| Symptom | Check and next action |
| --- | --- |
| `pvisor: command not found` | Activate the installation environment or put the native package's `bin` on `PATH`; see [installation checks](installation.md#verify-installation). |
| FUSE, FSKit, or mount failure | Complete [platform setup](installation.md#local-prerequisites). Check the device and mount helper on Linux or the filesystem extension on macOS. |
| `setgroups`, `uid_map`, or user namespace permission error | Check system user namespace and AppArmor policies; see the [Host executor](../guides/executors/host.md). |
| `last` is missing or selects another task | Return to the launch directory and use the recorded Job identity; see [Jobs and storage](../reference/jobs.md). |
| `apply` refuses to run | Check that the Job stopped, then inspect conflicts or incomplete records; see [review and apply](../guides/review-apply.md). |

`--safe` refuses to start if required controls cannot be installed. Keep the flag while fixing setup: ordinary Host Jobs write directly to the original workspace by default.

## Next steps {#next-steps}

- [Repair tests with Claude Code](../guides/agents/claude-code.md#repair-task): use a reproducible small project, task prompt, and acceptance checks for a real agent handoff.
- [Local workflows](../guides/index.md): use this process in an existing project, validate candidate changes, and handle conflicts.
- [VM tool execution](infrastructure.md): execute tests in a Linux VM from a caller script and read structured feedback.
- [CLI reference](../reference/cli.md): look up flags, Job selection, and storage rules.
