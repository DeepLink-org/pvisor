<a id="execution-infrastructure-run-and-manage-a-vm"></a>

# Execution infrastructure: tool execution integration example {#first-task}

Run tool commands in a VM through the pVisor CLI and return their results to an Agent, evaluator, or training workflow.

The example uses a candidate patch and two tool commands from the `qutebrowser-667` SWE trajectory: run targeted tests, then check the modified module's syntax. Both commands share one VM and workspace, and their results are returned together when execution finishes. No model API configuration is needed.

## 1. Prepare the environment {#prerequisites}

Complete the [execution infrastructure setup](installation.md#vm-and-oci), then run the example from the repository root on that node. If you do not have the repository yet:

```bash
git clone https://github.com/DeepLink-org/pvisor.git
cd pvisor
```

The repository includes the caller scripts and task inputs; the image supplies task dependencies.

Use Bash for the commands below. In the same terminal with the pVisor installation environment activated, check the tools:

```bash
pvisor --version
python3 --version
git --version
bash --version
test -r /dev/kvm && test -w /dev/kvm && echo 'KVM accessible'
```

Expect version information and `KVM accessible`. Resolve missing prerequisites through [installation](installation.md#vm-and-oci) before continuing. The Python caller runs on the host; task tests run inside the VM, in a separate environment.

## 2. Run the example {#start-task}

```bash
bash examples/pvisor/07-vm-task/run.sh
```

The first run prepares and caches a pinned task image; download and extraction determine preparation time. Later runs can reuse the image cache. No manual VM startup is needed. The example uses 1 vCPU and 1 GiB guest RAM with guest networking disabled. Each tool command has a 180-second limit and the guest script a 300-second limit; image preparation is outside that deadline.

On success, the terminal shows:

```text
Step 42: exit_code=0, timed_out=False
...
7 passed
Step 43: exit_code=0, timed_out=False
Syntax OK
```

The final lines should include `Job: …; Attempt: …` and `Results: …/sample.json`. Paths vary between runs; retain this run's `Results` path.

## 3. Open and check feedback {#read-feedback}

Read the records in the same terminal. Run the following and paste the complete path after `Results:` at the prompt, excluding the `Results:` label itself:

```bash
read -r -p 'Paste Results path: ' results_path
python3 -m json.tool "$results_path"
```

Expect task and execution identities and two tool calls in `steps`. Check each `exit_code` is 0 and `timed_out` is false, then find `7 passed` and `Syntax OK` in `stdout`. Successful feedback should match the terminal summary.

The records directory also contains `bundle.json`, the pVisor execution record. The caller reads tool feedback from `sample.json`. Passing the selected tests and syntax check does not establish that the full SWE task or all original requirements passed.

## 4. Check cleanup and failures {#cleanup}

When every tool succeeds, the adapter collects and saves feedback before releasing the stage. The records directory remains. Do not apply/drop `last` afterward: this example manages its own task and stage, and does not automatically apply the candidate patch to a host project.

Nonzero tool exits or timeouts retain feedback and stage, and the caller exits unsuccessfully. VM, preparation, or collection errors show `Execution error`. Keep the error and available records, then follow the [example error contract](https://github.com/DeepLink-org/pvisor/tree/main/examples/pvisor/07-vm-task) before cleanup. Avoid addressing an unrelated task through `last`.

| Symptom | Next action |
| --- | --- |
| Image download fails | Check Docker Hub DNS, access, and cache disk space. Offline setups need the [same task image's rootfs](../guides/executors/vm.md#prepare-a-rootfs). |
| KVM or namespace permission error | Fix the [VM prerequisites](installation.md#vm-and-oci). |
| Tool times out or tests fail | Inspect that step's output, exit code, and timeout state in `sample.json`; distinguish tool failures from execution infrastructure errors. |
| Cleanup fails | Feedback was saved first. Retain records and the error, then inspect stage state using the example README. |

The example README records a Linux/KVM run using an offline rootfs. A fresh registry download and other environments still need verification.

<a id="framework-integration"></a>

## 5. Use it in your calling workflow {#collect-results}

In a cluster, the framework dispatches tasks, a Worker calls pVisor to execute tools, and feedback returns to the framework. This example provides caller logic and an execution adapter that can run inside a Worker.

`run.py` prepares the task and handles results; `executor.py` executes the task through the CLI. The caller submits the task with:

```python
feedback = execute(task, EXAMPLE / "candidate.patch")
```

Reuse this structure with your task image, candidate patch and tool commands, then send the returned feedback to an Agent, evaluator or training-data collection workflow.

The example's `execute` adapter synchronously executes a fixed batch of commands. It does not include remote scheduling or an Agent loop that generates the next call from feedback.

See the [example README](https://github.com/DeepLink-org/pvisor/tree/main/examples/pvisor/07-vm-task) for task formats, error handling and cleanup rules.

<a id="pause-resume"></a>
<a id="offload-resume"></a>

Next: [Pause, offload and resume a VM](../guides/infrastructure/lifecycle.md) · [Runtime and Daemon integration](../guides/infrastructure/integration.md)
