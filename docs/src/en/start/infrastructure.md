<a id="execution-infrastructure-run-and-manage-a-vm"></a>

# Run tools in a VM {#first-task}

Run targeted tests and a syntax check for a real `qutebrowser-667` SWE trajectory excerpt, then return the results to your caller. No model API configuration is needed.

The example applies a candidate patch and runs two tool commands in one VM and workspace. Results return together when the batch finishes.

## 1. Prepare the environment {#prerequisites}

Prepare a **Linux x86_64 node** using the [VM prerequisites](installation.md#vm-and-oci). Run the example from the repository root on that node; if you do not have the repository yet:

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


<a id="framework-integration"></a>

## 3. Return feedback to your caller {#collect-results}

`run.py` prepares the task and handles results; `executor.py` runs it through the CLI. This call connects them:

```python
feedback = execute(task, EXAMPLE / "candidate.patch")
```

Replace the task image, candidate patch and tool commands, then pass the feedback to your Agent, evaluator or training-data collector. The caller and adapter can also run inside a framework's Worker.

The example's `execute` adapter synchronously executes a fixed batch of commands. It does not include remote scheduling or an Agent loop that generates the next call from feedback.

See the [example README](https://github.com/DeepLink-org/pvisor/tree/main/examples/pvisor/07-vm-task) for task formats, error handling and cleanup rules.

<a id="pause-resume"></a>
<a id="offload-resume"></a>

Next: [Pause, offload and resume a VM](../guides/infrastructure/lifecycle.md) · [Runtime and Daemon integration](../guides/infrastructure/integration.md)
