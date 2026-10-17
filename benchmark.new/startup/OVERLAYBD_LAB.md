# OverlayBD 实验部署与功能验证

本部署为 [B-LAZY-STARTUP](../../benchmark/README.md#b-lazy-startup) 的
[TurboOCI 对照](TURBO_OCI.md)准备可运行后端。使用官方发布程序，服务配置和数据
保存在独立 Docker 实验环境中；宿主现有 containerd、Docker 与 systemd 配置不变。

**OverlayBD、snapshotter、containerd 和私有 registry 已启动；普通 OCI 与
TurboOCI shell 功能验证均通过，另完成同源预准备 rootfs 的 pVisor VM 验证。**
这里是部署与正确性验证，不发布启动时间、加速倍数或容量成绩。

## 制品与运行环境

| 项目 | 实际值 |
| --- | --- |
| OverlayBD | 官方 v1.0.18，`overlaybd-1.0.18-20260710.cee2186.azurelinux.3.0.x86_64.rpm` |
| OverlayBD 包 SHA-256 | `79e75035ad7f3294212824a01838f31a560f784a2bdd7c5798a7c2ca6446d73e` |
| snapshotter / convertor / 专用 ctr | 官方 v1.4.5，`overlaybd-snapshotter-1.4.5-20260923091436.940d0ab.x86_64.rpm` |
| snapshotter 包 SHA-256 | `bc262e108c63cb4b152724e2eb74af8925a194e3e65fc8b928af146a611d22fc` |
| 用户态依赖 | 复制并记录本机 GNU 动态依赖；未将其他发行版包安装进宿主 |
| 基础镜像 | 本地 `registry:3`；实际镜像 ID、Dockerfile 和构建产物保留在实验目录 |
| 宿主内核 | Linux `7.2.10-200.fc44.x86_64` |
| 私有 containerd / runc | 本机冻结的 containerd 2.3.5 / runc 1.5.2 |
| 实验容器 | `pvisor-overlaybd-f8ab5eb38660` |
| 私有 registry | `http://127.0.0.1:55118` |
| 实验目录 | `benchmark.new/startup/.data/overlaybd-lab-20261017/` |

程序来自 [OverlayBD 发布页](https://github.com/containerd/overlaybd/releases/tag/v1.0.18)
与 [snapshotter 发布页](https://github.com/containerd/accelerated-container-image/releases/tag/v1.4.5)。
RPM 先在项目私有目录解包，再组装进实验镜像。`deployment-artifacts.json` 保存包摘要与
环境；`lab.json` 保存逐文件程序/动态库来源、摘要和最终 Docker 镜像 ID。
这些收据不声称程序由当前项目源码构建。

## 宿主内核接口与资源范围

已通过临时管理容器加载宿主已有的标准 `target_core_user` 和 `tcm_loop` 模块；
没有安装第三方驱动或修改自动加载配置。实验容器使用 `--privileged`、
`--cgroupns=host`，绑定宿主 `/dev` 与 `/sys/kernel/config`，并绑定私有目录到 `/lab`。
registry 仅发布到宿主 loopback，容器不使用 host network。

**TCMU 注册与设备属于宿主全局内核状态。** 此环境不能与另一 OverlayBD TCMU
实例并行使用。启动脚本检查已有进程和真实 user-backed targets，遇到占用就拒绝启动；
这不构成对其他管理工具的全局互斥锁，外部并发启动仍需协调。
空 HBA 的 `hba_info`、`hba_mode` 属性文件不当作真实目标。

| 资源 | 私有路径或策略 |
| --- | --- |
| OverlayBD 配置、缓存、日志 | `/lab/overlaybd.json`、`/lab/cache/`、`/lab/log/` |
| snapshotter | `/lab/snapshotter.json`、`/lab/run/overlaybd.sock` |
| containerd | `/lab/containerd.toml`、`/lab/containerd-root/`、`/lab/run/containerd/` |
| containerd socket / namespace | `/lab/run/containerd.sock` / `pvisor-obd-smoke` |
| 后台整层下载 | 显式关闭 `download.enable`，属于本实验配置 |
| 测试任务资源 | 2 CPU 配额、2 GiB 内存限制；服务进程尚未统一限额 |
| 测试 cgroup 前缀 | `/system.slice/pvisor-overlaybd-lab-f8ab5eb38660`；只清理本前缀内明确拥有的空目录 |

普通 OCI 容器与 TurboOCI 容器共用同一 runc/containerd。功能验证中的任务限制
不代表服务和缓存的整机资源已受同等约束，也不能与 VM 的 guest RAM 直接等同。

## 部署和操作入口

[overlaybd_lab.py](overlaybd_lab.py)负责环境准备、启动、状态、发布源镜像和受检查的
停止；[overlaybd_smoke.py](overlaybd_smoke.py)负责同源 OCI/TurboOCI 功能验证。
它们需要当前用户可操作 Docker；涉及设备和 namespace 的检查必须在真实宿主权限下运行。

已执行的准备与发布入口如下；`prepare` 拒绝复用已存在的实验目录。

```sh
python3 benchmark.new/startup/overlaybd_lab.py prepare \
  --lab benchmark.new/startup/.data/overlaybd-lab-20261017 \
  --overlaybd-root benchmark.new/startup/.data/turbo-oci-prerequisites-20261017T124852Z/extracted-azurelinux3 \
  --snapshotter-root benchmark.new/startup/.data/turbo-oci-prerequisites-20261017T124852Z/extracted
python3 benchmark.new/startup/overlaybd_lab.py start \
  --lab benchmark.new/startup/.data/overlaybd-lab-20261017
python3 benchmark.new/startup/overlaybd_lab.py publish \
  --lab benchmark.new/startup/.data/overlaybd-lab-20261017 \
  --oci-layout benchmark/pvisor/.data/lazy-shell-current-formal-20261010/archive/registry-oci
```

`publish` 校验 OCI layout 的 manifest、config 和每个原始 layer 的 SHA-256/大小，
逐字节发布到私有 registry，不从 rootfs 重打镜像。
使用官方 convertor 的实际容器内命令：

```sh
/opt/overlaybd/snapshotter/convertor \
  -r 127.0.0.1:55118/pvisor-lab/shell \
  -g sha256:88a381d5b5eeb2b35d3ad70925a362c37ce569daf43ede89ff818ec20e4d3794 \
  --turboOCI turbo -d /lab/conversion --plain --reserve --dump-manifest
```

源 digest 为 `sha256:88a381d5b5eeb2b35d3ad70925a362c37ce569daf43ede89ff818ec20e4d3794`；
TurboOCI 派生 digest 为 `sha256:2db913afbb14ee34ccf158ef310913dca4c3739c493b6e2d35258520c2721526`。
源与派生 manifest 分别保存，运行命令使用 digest 而不依赖移动 tag。

本 registry 使用 HTTP，三个入口分别设置：convertor 的 `--plain`、
专用 ctr 的 `rpull --plain-http`，以及 snapshotter 的
`mirrorRegistry=[{"host":"127.0.0.1:55118","insecure":true}]`。
`rpull` 标志不改变 snapshotter 生成的 `repoBlobUrl`；变更该配置后须重新生成 snapshots。
`rpull` 来自包内专用 ctr，不能用缺少该命令的系统 ctr 替换。

首次在 Docker 私有 cgroup namespace 内执行嵌套 runc 时，父组为 threaded，
内存限制创建失败。改用 host cgroup namespace，并让任务落到本实验独立的 domain
cgroup 后，同一 TurboOCI shell 正常运行。失败输入和日志保留在实验目录，未纳入性能结果。

## 功能验证与状态检查

```sh
python3 benchmark.new/startup/overlaybd_smoke.py \
  --lab benchmark.new/startup/.data/overlaybd-lab-20261017
python3 benchmark.new/startup/overlaybd_lab.py status \
  --lab benchmark.new/startup/.data/overlaybd-lab-20261017
```

smoke 验证固定 Ubuntu 26.04 镜像、唯一 `LAZY_READY ubuntu 26.04` 输出和退出码，
记录实际 snapshotter、预算、设备、挂载和清理状态。源 OCI 与 TurboOCI 的缓存
可能已在部署中使用，不能把这些运行称为客户端冷启动，或据此计算加速倍数。

已完成的容器 smoke 证据位于实验目录
`smoke-20261017T130601Z-29ffc376/`，其中 `result.json` 记录两组通过：

| 验证 | 结果 |
| --- | --- |
| 同源 OCI / kernel overlayfs | 正确 marker、退出 0、实际 snapshotter 与 image 匹配 |
| TurboOCI / OverlayBD / TCMU | 正确 marker、退出 0，运行中观察到实际 TCMU 目标和块设备 |
| 运行中的资源约束 | 两组均读回 `memory.max=2147483648`，`cpu.max` 配额为周期的 2 倍 |
| 退出后的资源 | 测试 task/container 消失，唯一 cgroup 前缀清空，TCMU/block/UIO 清单回到基线 |
| pVisor VM 同源功能基线 | 正确 marker、退出 0；Run Bundle 为 completed / virtual_machine，原有 Host 进程保留 |

pVisor 单次功能证据位于
`benchmark/pvisor/.data/overlaybd-pvisor-vm-functional-20261017-c00af616/`。
它使用同源预准备 rootfs，不验证 lazy 路径，不代表包含拉取/解包的 `oci-vm`
冷启动条件；与容器 smoke 不合并成性能对比。

服务保留运行以供后续实验。停止前须通过私有 ctr 清理本实验的任务、容器、镜像、
lease 和 snapshots，核对 TCMU 设备与挂载已释放，再调用：

```sh
python3 benchmark.new/startup/overlaybd_lab.py stop \
  --lab benchmark.new/startup/.data/overlaybd-lab-20261017
```

`stop` 审计该私有 containerd 的全部 namespaces 和健康 snapshotters；有残留就拒绝
自动删除。它不运行上游全局清理脚本、不清空宿主页缓存，也不删除其他实例的 configfs 对象。
异常退出时保留现场和恢复提示。正常停止后保留私有数据与日志，宿主标准模块仍保持加载。
每次新启动需要新的资源基线，不能将旧观察伪装成新启动前状态。

正式性能对比仍需接通独立采样、局部冷/热缓存、Range 206 实际传输计数和审计，
按 [四组协议](TURBO_OCI.md)另建批次；部署 smoke 不能替代这一步。
