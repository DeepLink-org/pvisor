<a id="execution-infrastructure-run-and-manage-a-vm"></a>

# Execution infrastructure: run your first task {#first-task}

Run the repository's VM task example on a Linux execution node to submit a task and collect its result. Evaluators, training frameworks and schedulers can reuse this flow without deploying a Daemon first.

## 1. Prepare the environment {#prerequisites}

Complete the Linux/KVM node setup in [Installation](installation.md#vm-and-oci) and activate the Python environment where pVisor is installed. Run all commands below on that execution node.

The repository supplies the scripts; no manual script creation is needed. If you already have the current repository, enter its root directory. Otherwise:

```bash
git clone https://github.com/DeepLink-org/pvisor.git
cd pvisor
```

The example selects a Python image containing its task dependencies. pVisor downloads and caches it on first use; no Docker installation or manual rootfs preparation is required. Initial preparation needs registry access; see the [VM executor](../guides/executors/vm.md#prepare-a-rootfs) for offline preparation.

## 2. Run the example {#start-task}

```bash
bash examples/pvisor/07-vm-task/run.sh
```

The example reads the inputs `20` and `22`, computes their sum in a VM and writes the result. Its files have separate roles:

| File | Responsibility |
| --- | --- |
| `run.py` | Host caller: submit the task, check execution status, collect results and release the stage |
| `input.json` | Task input |
| `task.py` | Task program executed inside the VM |

The task uses 1 vCPU and 256 MiB guest RAM with guest networking disabled. It makes no model API calls.

## 3. Read the result {#collect-results}

On success, the terminal prints `result.answer: 42` and a `Records` directory for this run. The directory is under the example's `.work/` and contains:

- `sample.json`: the caller's task ID, Job/Attempt IDs and task result.
- `bundle.json`: execution state, exit code and execution record.

Successful collection releases the stage; failed execution retains it for inspection.

To integrate your own task, replace `input.json` and `task.py`, then have the caller read `sample.json` for scoring or further scheduling. Change the example's image configuration when other tools are needed. Execution parameters and result collection live in `run.py`.

<a id="pause-resume"></a>
<a id="offload-resume"></a>

Next: [Sandbox lifecycle management](../concepts/lifecycle.md#cli-example) · [Runtime and Daemon integration](../guides/infrastructure/integration.md)
