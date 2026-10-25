# Tool execution integration example / 工具执行接入示例

A caller script submits a fixed task through an adapter over the pVisor CLI.
Execution and feedback collection are verified; no actual Agent framework or
worker service is integrated. Such integration remains future example work.
Replay two recorded tool calls from `qutebrowser-667` in a pVisor VM: run a targeted
pytest selection, then compile the modified module. The candidate patch is taken
from the recorded Agent run. No live model, artificial reasoning waits, GPU,
Docker Daemon or source build is required.

调用脚本通过 pVisor CLI 适配函数提交固定任务，已验证执行与反馈收集。
当前未接入真实 Agent 框架或实现 Worker 服务，相关集成示例待补充。在 pVisor VM 中执行 `qutebrowser-667` 原轨迹的两次工具调用：运行针对性测试，再检查
修改后的模块语法。候选补丁来自已记录的 Agent 执行，不调用模型，不模拟推理等待。

## Run / 运行

After installing pVisor and configuring Linux x86_64/KVM permissions, run from
the repository root in the active Python 3.10+ environment:

完成 pVisor 安装和 Linux x86_64/KVM 权限配置后，在已激活的 Python 3.10+ 环境中，
从仓库根目录运行：

```bash
bash examples/pvisor/07-vm-task/run.sh
```

pVisor prepares and caches the pinned SWE task image; it supplies Python, pytest,
Qt and the repository. Initial download requires registry access and sufficient
disk space. The example uses 1 vCPU and 1 GiB guest RAM, disables guest networking,
and limits each tool call to 180 seconds and the guest runner to 300 seconds.
Image preparation is outside the execution deadline. Python import bytecode caches
are disabled to avoid incidental writes; the explicit syntax-check command still runs.

pVisor 准备并缓存固定摘要的 SWE 任务镜像，镜像包含 Python、pytest、Qt 和项目代码。
首次下载需要访问镜像仓库和足够磁盘空间。示例使用 1 vCPU、1 GiB guest RAM，关闭
guest 网络；每次工具调用限时 180 秒，VM 内脚本限时 300 秒，镜像准备时间另计。禁用 Python 导入时的字节码缓存以减少无关写入；显式语法检查命令仍照常执行。

## Files and integration / 文件与接入

| File | Responsibility / 职责 |
| --- | --- |
| `task.json` | Image, base revision, environment and original tool calls / 镜像、代码基线、环境与原始工具调用 |
| `candidate.patch` | Recorded candidate modification / 轨迹中的候选修改 |
| `task.py` | Guest-side preparation and tool execution / VM 内准备代码并执行工具 |
| `run.py` | Caller script: submit inputs and consume observations / 调用脚本：提交输入、消费反馈 |
| `executor.py` | Example adapter: execute through the CLI and return feedback / 示例适配函数：通过 CLI 执行并返回反馈 |

The guest checks out the recorded base and applies the candidate patch before the
selected calls. It also supplies the guest-local `localhost` hosts entry normally
injected by an OCI runtime, needed by pytest’s local coordination socket. These
are preparation steps, not additional recorded Agent calls.
The image stays unchanged: `/app` changes belong to the VM's temporary rootfs layer;
feedback is written into the staged caller workspace and collected before cleanup.

VM 先检出记录的代码基线并应用候选补丁，重建片段执行前的状态。这些属于准备步骤，
不计入两条原始工具调用。VM 内还补齐通常由 OCI Runtime 注入的 `localhost` 本机映射，
供 pytest 插件使用本地协调套接字。`/app` 的修改位于 VM 临时 rootfs 写层，不改写镜像；反馈
写入调用方工作区的 stage，收集后再清理。

The call in `run.py` is / `run.py` 中的调用入口为：

```python
feedback = execute(task, EXAMPLE / "candidate.patch")
```

`execute` is an example adapter, not an official SDK. It takes a task dictionary
and patch path, then returns a JSON-serializable dictionary with `task_id`,
`sample_id`, `job_id`, `attempt_id`, `records_dir` and `steps`. Each step preserves
its command and tool-call ID alongside stdout, stderr, exit code and timeout status.
It saves the same feedback in `sample.json` before discarding successful stages.

`execute` 是示例适配函数，不是官方 SDK。输入为任务字典和补丁路径，返回可序列化为
JSON 的字典，包含任务、样本、Job／Attempt 身份、记录目录及 `steps`。每一步保留
命令与工具调用 ID，以及输出、退出码和超时状态；清理成功任务的 stage 前，先将
同样的反馈保存为 `sample.json`。

The caller receives all observations after the fixed command batch finishes.
A future framework example should connect an actual tool-execution interface,
issue subsequent calls from feedback in the same environment, and define cleanup
on completion, failure and cancellation. A worker belongs to that framework;
cross-node placement and scoring remain outside this adapter.

调用脚本在固定命令批次结束后一次性收到反馈。后续框架示例需要对接实际工具执行接口，
展示根据反馈在同一环境继续调用，并明确任务结束、失败和取消时的回收方式。
Worker 属于框架；跨节点调度和评分不由此适配函数负责。

## Results / 结果

Expected feedback: step 42 exits 0 with `7 passed`; step 43 exits 0 with `Syntax OK`.
The printed `Records` directory under `.work/` contains `sample.json` (task,
Job/Attempt IDs and each call's stdout/stderr/exit code/timeout status) and
`bundle.json` (VM execution record). No fixed elapsed time is promised.

预期反馈：第 42 步退出码为 0、显示 `7 passed`；第 43 步退出码为 0、显示 `Syntax OK`。
`.work/` 下的 `Records` 目录保存 `sample.json`（任务与执行身份、每条命令的输出、
退出码和超时状态）及 `bundle.json`（VM 执行记录）。不以原轨迹耗时作为性能承诺。

A nonzero tool exit or tool timeout remains a returned observation; the adapter
retains feedback and stage. The example caller chooses to exit unsuccessfully
when these observations contain failures; a framework can instead use them for
its next decision. VM/runner/collection errors raise `ExecutionError`, preserving
available artifacts. Cleanup errors also raise it after saving feedback.
Successful calls release the stage after collection.

工具返回非零退出码或超时时，适配器仍返回反馈并保留 stage。示例调用方选择返回失败；
实际框架可消费这些反馈并决定下一步。VM、任务准备或反馈收集失败抛出 `ExecutionError`
并保留已有产物；清理失败在保存反馈后单独报错。全部工具成功时，收集反馈后释放 stage。

## Provenance and scope / 来源与范围

- Source: mini-SWE-agent baseline run, 2026-10-21, `qutebrowser-667`; tool calls
  42–43 (one-based Agent calls; excludes the harness's `uname -r` probe).
- Trajectory SHA256: `77a60b3a22511c1f387a6f92c4753d3c1f70bbb0aca444037729b060554a53ff`.
- Candidate patch SHA256: `c821d2fdba22a900f1a27af14c341c2c1ec0760e0d67fce845b2eae912c129ab`.
- Base commit: `a67832ba311fdb0e9d57190d1671241a369b5b0a`. Pinned OCI manifest and
  original tool-call IDs are recorded in `task.json`.
- The patch modifies qutebrowser's GPL-3.0-or-later `webview.py`. The task image
  supplies the upstream source; `LICENSE.qutebrowser` retains its license, and
  the candidate is reproduced unchanged. Copyright: Florian Bruhin (The Compiler).

The selected tests exercise suffix behavior through the class compatibility entry.
Passing them does not establish that every requirement of the original refactoring
was satisfied. The Qt application tests that aborted in the source run are excluded;
this is neither full SWE evaluation nor evidence that those tests now pass.

选中的测试通过类上的兼容入口检查后缀处理行为，不能据此断言原重构要求全部满足。
原轨迹中发生中止的 Qt 应用测试不在片段内；本示例不代表完整 SWE 评测通过。

## Optional configuration / 可选配置

| Variable | Purpose / 用途 |
| --- | --- |
| `PVISOR_BIN` | CLI path, read by this example / 此示例使用的 CLI 路径 |
| `WORK_ROOT` | Parent directory for unique run outputs / 每次执行的输出父目录 |
| `IMAGE_STORE` | Persistent OCI cache / OCI 镜像缓存 |
| `ROOTFS` | Offline rootfs from the same task image, including its Git repository / 同一任务镜像的离线 rootfs，需包含 Git 仓库 |

This example runs independently of `mise run examples`.
该示例独立运行，不加入 `mise run examples`。

## Validation / 验证

Verified on 2026-10-25 on gpu03 (Linux x86_64/KVM), CLI
`0.3.0+g35.b318715`: both original calls exited 0, with `7 passed` and `Syntax OK`;
the caller script received both observations and Job/Attempt IDs.
The prepared base repository stayed clean.
This run used an offline copy of the recorded task image's initialized rootfs.
A fresh registry download was not verified because gpu03 could not resolve Docker Hub.

2026-10-25 在 gpu03（Linux x86_64/KVM）、CLI `0.3.0+g35.b318715` 上验证：两条原始
命令退出码均为 0，分别输出 `7 passed` 与 `Syntax OK`；调用脚本收到两条反馈和 Job／Attempt ID，
基础代码仓库保持未修改。本次使用原任务镜像已初始化 rootfs 的离线副本；
gpu03 无法解析 Docker Hub，未验证首次在线拉取。

Adapter contract checks (no VM required) / 适配器契约检查（无需启动 VM）：

```bash
python3 -m unittest discover -s examples/pvisor/07-vm-task -p test_executor.py
```

- [中文快速开始](../../../docs/src/zh/start/infrastructure.md)
- [English quick start](../../../docs/src/en/start/infrastructure.md)
