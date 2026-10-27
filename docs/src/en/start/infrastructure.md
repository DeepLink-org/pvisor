<a id="execution-infrastructure-run-and-manage-a-vm"></a>

# Execution infrastructure: tool execution integration example {#first-task}

In Agent inference, evaluation or training workflows, callers send tool commands to an execution environment and use the results to decide what to do next. This example shows how to execute tools in a VM through the pVisor CLI and collect feedback.

The example uses a candidate patch and two tool commands from the `qutebrowser-667` SWE trajectory: run targeted tests, then check the modified module's syntax. Both commands share one VM and workspace, and their results are returned together when execution finishes. No model API configuration is needed.

## 1. Prepare the environment {#prerequisites}

Complete the [execution infrastructure setup](installation.md#vm-and-oci), then run the example from the repository root on that node. If you do not have the repository yet:

```bash
git clone https://github.com/DeepLink-org/pvisor.git
cd pvisor
```

The repository includes the caller scripts and task inputs; the image supplies task dependencies.

## 2. Run the example {#start-task}

```bash
bash examples/pvisor/07-vm-task/run.sh
```

On success, the terminal shows:

```text
Step 42: exit_code=0, timed_out=False
...
7 passed
Step 43: exit_code=0, timed_out=False
Syntax OK
```

The terminal also prints this task's records directory, containing:

- `sample.json`: each tool command's output, exit code, timeout status and associated execution IDs.
- `bundle.json`: the pVisor execution record.

This example shows how to execute a SWE tool-call excerpt and collect feedback by running targeted tests and a syntax check.

<a id="framework-integration"></a>

## 3. Use it in your calling workflow {#collect-results}

In a cluster, the framework dispatches tasks, a Worker calls pVisor to execute tools, and feedback returns to the framework. This example provides caller logic and an execution adapter that can run inside a Worker.

`run.py` prepares the task and handles results; `executor.py` executes the task through the CLI. This call connects them:

```python
feedback = execute(task, EXAMPLE / "candidate.patch")
```

Reuse this structure with your task image, candidate patch and tool commands, then send the returned feedback to an Agent, evaluator or training-data collection workflow.

The example's `execute` adapter synchronously executes a fixed batch of commands. It does not include remote scheduling or an Agent loop that generates the next call from feedback.

See the [example README](https://github.com/DeepLink-org/pvisor/tree/main/examples/pvisor/07-vm-task) for task formats, error handling and cleanup rules.

<a id="pause-resume"></a>
<a id="offload-resume"></a>

Next: [Pause, offload and resume a VM](../guides/infrastructure/lifecycle.md) · [Runtime and Daemon integration](../guides/infrastructure/integration.md)
