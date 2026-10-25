# VM task / VM 任务

Run an existing caller against a Linux/KVM execution node. The Host runs `run.py`;
the VM runs `task.py` with `input.json`. No model API, GPU, Docker daemon, or Rust
build is needed. Install pVisor into an active Python 3.10+ environment first;
the account must have KVM and user namespace permissions.

在 Linux/KVM 执行节点直接运行仓库示例：Host 上的 `run.py` 提交任务，VM 中的
`task.py` 读取 `input.json` 并生成结果。先完成 pVisor 安装和节点权限配置；
不需要模型 API、GPU、Docker Daemon 或 Rust 编译。

## Run / 运行

From the repository root / 在仓库根目录执行：

```bash
bash examples/pvisor/07-vm-task/run.sh
```

pVisor prepares a digest-pinned Linux amd64 `python:3.12-slim` image and reuses its
cache. The image includes guest Python; Host Python comes from the installation
environment. Initial preparation requires access to Docker Hub. Guest networking
is disabled; the task gets 1 vCPU, 256 MiB guest RAM and a 60-second execution limit.
Image preparation time is separate from that limit.

pVisor 自动准备并缓存固定摘要的 Linux amd64 `python:3.12-slim` 镜像；VM 中的
Python 由镜像提供，Host Python 使用安装时的环境。首次准备需要访问 Docker Hub。
任务关闭 guest 网络，使用 1 vCPU、256 MiB guest RAM，执行超时为 60 秒；镜像准备
时间不包含在此执行超时内。

## Result / 结果

The console prints `result.answer: 42` and a unique `Records` directory under
`.work/`. `sample.json` associates the caller's task ID, Job/Attempt IDs and result;
`bundle.json` preserves the execution record. Successful collection drops the stage
without applying changes to the input workspace. Failure retains the stage.

终端输出 `result.answer: 42` 和 `.work/` 下本次运行的 `Records` 目录。
`sample.json` 保存任务与执行身份及结果，`bundle.json` 保存执行记录。
成功收集后清理 stage，不合入输入工作区；失败时保留现场。

## Adapt / 接入自己的任务

Replace `input.json` and `task.py`, then adapt `run.py` for your scheduler's task
ID and result handling. Select another image if your task needs other tools.
Retries, scoring and cross-node scheduling remain with the caller.

替换 `input.json` 与 `task.py`，再按调度程序的任务 ID 和结果处理方式修改 `run.py`；
任务需要其他工具时更换镜像。重试、评分和跨节点调度由调用方负责。

## Optional configuration / 可选配置

| Variable | Purpose / 用途 |
| --- | --- |
| `PVISOR_BIN` | Installed binary override, read by this example only / 仅此示例读取的 CLI 路径 |
| `WORK_ROOT` | Parent directory for unique task outputs / 每次任务的输出父目录 |
| `IMAGE_STORE` | Persistent OCI cache location / OCI 镜像缓存位置 |
| `ROOTFS` | Offline, pre-extracted Linux amd64 rootfs from the same image / 离线时使用同一镜像预先解包的目录 |

These examples are standalone; `07-vm-task` is not part of `mise run examples`.
该示例独立运行，不加入 `mise run examples` 的默认或命名场景。

Validation: Linux x86_64/KVM on gpu03, CLI `0.3.0+g35.b318715`, using the pinned
image from a transferred OCI cache. A cold online pull could not complete because
gpu03 could not resolve Docker Hub; it is not reported as passing.

验证环境为 gpu03、Linux x86_64/KVM、CLI `0.3.0+g35.b318715`，使用已传入的固定
摘要 OCI 镜像缓存。gpu03 无法解析 Docker Hub，首次在线拉取未通过验证。

- [中文快速开始](../../../docs/src/zh/start/infrastructure.md)
- [English quick start](../../../docs/src/en/start/infrastructure.md)
