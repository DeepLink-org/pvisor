# pVisor Job 用户场景与回归示例

从最简单的 Job 开始，逐步加入资源限制、stage、VM、容器和网络功能，最后走完审查、分支与环境复用流程。
每个 case 先说明用途、准备和预期结果，再给出可执行命令；编号便于单独回归。
`run` 创建 Job；`status`、`inspect`、`apply`、`drop`、`fork`、`kill` 直接操作 Job。`env` 管理可复用环境，`replay` 从轨迹启动 Job。编号（如 A01）只用于回归报告和问题定位。



## 按需求选择

| 你的需求 | 建议先看 |
|---|---|
| 只想运行一个命令，或确认默认写入 | A01–A03、A07 |
| 需要超时、内存或文件限制 | A04、B01–B04 |
| 想保留、丢弃或检查文件改动 | C01–C06 |
| 需要组合 OverlayFS 层或授予路径权限 | D01、D04–D06 |
| 想了解 host 默认隔离 | D02–D03 |
| 使用 VM、宿主 rootfs 或 OCI 镜像 | E01–E06 |
| 使用原生 OCI 容器 | F01–F04 |
| 配置网络代理或禁止网络 | G01–G07 |
| 接入 Gateway 或记录轨迹 | H01–H02 |
| 从配置文件或 RunSpec 执行 | I01–I03 |
| 参考多能力组合 | J01–J03 |
| 审查、选择性提交、分支或终止 Job | K01–K04 |
| 复用环境或准备轨迹回放 | L01–L02、M01 |
| 验证终端界面与权限弹窗 | D06、M02 |

每个场景保留用户命令、用途和预期；正式执行检查已迁入
`tests/semantics/documented-cases.md`，使用 semspec 运行。原 A01–M02 文档编号
保留用于查找，每个场景下方标有对应的 S-DOC ID。

## 如何使用

手工执行时，先准备一个测试工作目录。各例中的 `pvisor` 应已在 `PATH` 中；
`/path/to/...` 需要替换成自己的路径或镜像引用。

```bash
mkdir -p /tmp/pvisor-cases/workspace
cd /tmp/pvisor-cases/workspace
```

从仓库根目录运行自动检查：

```bash
just semspec list --domain DOC
just cases --case S-DOC-001,S-DOC-012 --keep
just cases
just semspec show S-DOC-001
```

`just cases` 构建 release pVisor，运行全部 56 个 DOC 规格，输出 JSON 报告到
`target/pvisor-case-report.json`。使用 S-DOC ID 选择 case；A01 对应 S-DOC-001，
C01 对应 S-DOC-012。完整映射见 `tests/semantics/README.md`。也可以通过
`just semspec run --domain DOC --subject-bin PATH` 使用已有二进制。

每条规格把原来的命令、退出预期和全部断言放在同一个审核摘要内。预期非零退出必须
实际发生并通过原断言，不作为 xfail。断言词汇来自 sealed `cases.sh`，读取当前 case
的 `run-bundle.json`、`run.json` 和命令日志。pVisor 配置、Job 数据、环境存储及夹具都在
临时 CASE_ROOT；失败保留现场，`--keep` 保留所有现场。缺少声明的前提条件报告 SKIP。

新增规格保持 UNREVIEWED。检查成功不等于人工批准；人工完成规格、词汇和引擎审核后，
才使用 `just cases --require-reviewed` 作为门禁。semspec 支持的选项以 `just semspec run --help`
为准，旧 Python runner 的 `--list`、`--report`、`--run-unavailable` 和 `--strict-skips` 不再使用。

| 测试资源 | 环境变量 |
|---|---|
| Linux rootfs | `PVISOR_CASE_ROOTFS`；Linux 未设置时使用宿主 `/`，仅验证流程，不代表独立 guest rootfs |
| VM 镜像 | `PVISOR_CASE_IMAGE`；默认 `ubuntu:latest` |
| 容器镜像 | `PVISOR_CASE_CONTAINER_IMAGE`；默认 `ubuntu:latest`，与动态 pVisor ABI 兼容 |
| 自动选择的 OCI runtime | `PVISOR_CASE_CONTAINER_RUNTIME`；未设置时依次查找 crun/runc；F04 显式要求 runc |
| VM 内 Agent | `PVISOR_CASE_AGENT`；需在 guest 中可执行 |

Linux host stage 需要 user/mount namespace 和 FUSE，macOS stage 需要可用 macFUSE。
VM 示例需要 Linux 和 `/dev/kvm`。OCI runtime 可执行文件存在不保证具备运行容器的权限。
这些限制仍会由实际运行和断言检验，不自动当作测试通过。

## 可执行 Case

### A. 基础调用与身份

这一组适合第一次使用 pVisor。先从 A01 开始；只有需要固定显示名、采集输出或显式传递环境变量时，再选择后续例子。

- [ ] **A01：省略 `run` 的最简调用**

  建议场景：适合第一次使用 pVisor、确认命令和 Job 身份。

  用途：在当前目录执行一个命令，不需要显式写出 `run`。`--` 后全部是交给 Agent 的命令和参数。

  预期：输出当前 workspace 的绝对路径并成功退出。默认使用 host executor，不启用 stage；未配置网络策略时记录为 ambient。

  ```bash
  pvisor -- /bin/pwd
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-001`，命令为 `just cases --case S-DOC-001`。

- [ ] **A02：显式 `run` 与省略形式等价**

  建议场景：适合第一次使用 pVisor、确认命令和 Job 身份。

  用途：对比省略和显式写出 `run` 的两种调用。分别保存 Agent 的标准输出，便于比较。

  预期：两个输出文件内容相同，都是当前工作目录。两次运行会各自生成记录，Job ID 和时间可以不同。

  ```bash
  pvisor -- /bin/pwd > implicit.txt
  pvisor run -- /bin/pwd > explicit.txt
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-002`，命令为 `just cases --case S-DOC-002`。

- [ ] **A03：Job 名称和 stdio capture**

  建议场景：适合第一次使用 pVisor、确认命令和 Job 身份。

  用途：为这次运行命名，并将 Agent 输出保存到运行结果。`--name smoke` 指定显示名，`--stdio capture` 开启输出采集。

  预期：Job 名称为 `smoke`，结果中的标准输出为 `hello`，未被截断。

  ```bash
  pvisor --name smoke --stdio capture -- /bin/sh -c 'printf hello'
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-003`，命令为 `just cases --case S-DOC-003`。

- [ ] **A04：超时**

  建议场景：适合第一次使用 pVisor、确认命令和 Job 身份。

  用途：给运行设置墙钟超时。`100ms` 是从运行开始计时的持续时间，不是 CPU 时间；命令故意睡眠 10 秒。

  预期：pVisor 非零退出，运行结果的失败类型为 `deadline_exceeded`。

  ```bash
  pvisor --timeout 100ms -- /bin/sleep 10
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-004`，命令为 `just cases --case S-DOC-004`。

- [ ] **A05：严格执行模式拒绝 best-effort 边界**

  建议场景：适合第一次使用 pVisor、确认命令和 Job 身份。

  用途：要求严格执行能力检查。`--strict` 不接受所请求能力缺少强制执行证据；这里同时要求禁止网络。

  准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

  预期：当前 host / container / VM 执行路径在启动 Agent 前均因缺少 Subprocess
  enforcement 证据而拒绝请求（`UnsupportedPolicy`）。此例验证 fail-closed，
  不代表 `--strict` 当前在任一 executor 上可达“更强沙箱已就绪”。

  ```bash
  pvisor --strict --overlaynet-deny-all -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-005`，命令为 `just cases --case S-DOC-005`。

- [ ] **A06：显式环境投影**

  建议场景：适合第一次使用 pVisor、确认命令和 Job 身份。

  用途：只把指定的宿主环境变量传给子进程。变量仅为这条命令设置，通过 `--pass-env` 显式允许投影。

  预期：子进程可见 `TEST_PVISOR_VALUE=visible`；运行记录列出这个变量，但不声明整体继承宿主环境。

  ```bash
  TEST_PVISOR_VALUE=visible pvisor --pass-env TEST_PVISOR_VALUE -- /usr/bin/env
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-006`，命令为 `just cases --case S-DOC-006`。

- [ ] **A07：默认写入直接到 workspace**

  用途：验证普通 host Job 的默认可写 lower；无需为日常命令额外选择执行器或 stage。

  预期：命令退出后，`direct.txt` 直接出现在原 workspace，记录中没有 OverlayFS stage。

  ```bash
  pvisor -- /bin/sh -c 'printf direct > direct.txt'
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-007`，命令为 `just cases --case S-DOC-007`。

### B. 资源限制

这一组展示“请求限制”和“实际强制”之间的区别。B01 用于查看完整配置，B02 才真正尝试触发文件大小限制。

- [ ] **B01：组合使用所有资源限制**

  建议场景：适合需要控制或验证资源限制的任务。

  用途：组合设置内存、进程数、CPU 时间、打开文件数和单文件大小。`MiB` 是二进制单位；`--max-cpu-time` 与墙钟超时不同。

  预期：命令成功退出，五个请求值出现在运行记录中，同时报告生效值和限制机制。`/bin/true` 不消耗这些额度，此例不测试超限行为。

  ```bash
  pvisor \
    --memory 256MiB \
    --max-processes 32 \
    --max-cpu-time 5s \
    --max-open-files 128 \
    --max-file-size 1MiB \
    -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-008`，命令为 `just cases --case S-DOC-008`。

- [ ] **B02：文件大小限制实际生效**

  建议场景：适合需要控制或验证资源限制的任务。

  用途：验证单文件大小限制：将上限设为 1KiB，再尝试用 `dd` 写入 4KiB。

  预期：写入命令失败，落盘文件如果存在，其大小不超过 1024 字节；运行结果记录进程退出失败。

  ```bash
  pvisor --max-file-size 1KiB -- /bin/sh -c 'dd if=/dev/zero of=large bs=4096 count=1'
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-009`，命令为 `just cases --case S-DOC-009`。

- [ ] **B03：内存参数短别名**

  建议场景：适合需要控制或验证资源限制的任务。

  用途：使用 `--memory` 的别名 `--mem`，为一个简单命令设置 256MiB 内存额度。

  预期：命令成功，记录中的请求值为 268435456 字节，与 `--memory 256MiB` 一致。

  ```bash
  pvisor --mem 256MiB -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-010`，命令为 `just cases --case S-DOC-010`。

- [ ] **B04：Stage 总大小限制**

  建议场景：适合需要控制或验证资源限制的任务。

  用途：为持久 stage 请求 1GiB 的总大小限制。它限制的是 stage 总量，和 B02 的单个文件大小不是同一个概念。

  准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

  预期：stage 成功建立并保存在指定路径。此例只验证参数可用和目录建立；当前产物未记录该上限，也未在此例中尝试写满 stage。

  ```bash
  pvisor --stage /tmp/pvisor-cases/limited-stage --overlayfs-max-size 1GiB -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-011`，命令为 `just cases --case S-DOC-011`。

### C. Stage 与 whole-rootfs

当你希望 Agent 可以自由修改文件、但不污染当前 workspace 时使用这一组。C01 是最常用的持久模式；C02 使用 `--safe` 自动选择并保留 stage，C03 演示对持久 stage 显式执行 `drop`。

- [ ] **C01：持久 stage**

  建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

  用途：把本次运行的文件改动放进一个保留的 stage。命令在 workspace 里创建 `result.txt`。

  准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

  预期：原 workspace 没有 `result.txt`；变更清单中出现该文件，指定 stage 内保留 `run-bundle.json`，便于之后查看。

  ```bash
  pvisor --stage /tmp/pvisor-cases/stage-keep -- /bin/sh -c 'printf changed > result.txt'
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-012`，命令为 `just cases --case S-DOC-012`。

- [ ] **C02：默认保留 stage**

  建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

  用途：无需手写存储路径。`--safe` 在没有指定 `--stage` 时使用持久 Job 存储，退出后保留改动。

  准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

  预期：命令成功，日志中给出的存储目录及 Run Bundle 保留，原 workspace 没有新建的文件。

  ```bash
  pvisor --safe -- /bin/sh -c 'printf changed > result.txt'
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-013`，命令为 `just cases --case S-DOC-013`。

- [ ] **C03：显式丢弃持久 stage 的改动**

  建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

  用途：指定持久 stage 路径，完成运行后通过 `pvisor drop` 显式丢弃其中的改动。

  准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

  预期：stage 目录保留，运行记录的文件系统状态变为 `discarded`；原 workspace 没有新文件。

  ```bash
  pvisor --stage /tmp/pvisor-cases/stage-drop -- /bin/sh -c 'printf changed > result.txt'
  pvisor drop /tmp/pvisor-cases/stage-drop
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-014`，命令为 `just cases --case S-DOC-014`。

- [ ] **C04：显式 stage 保留已有目录内容**

  建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

  用途：验证 `--stage PATH` 始终表示持久目录；目录里已有的用户文件不会因运行结束而被删除。

  预期：命令成功，原有的 `user-file` 和新生成的 Run Bundle 都保存在指定目录。

  ```bash
  mkdir -p /tmp/pvisor-cases/existing-stage
  touch /tmp/pvisor-cases/existing-stage/user-file
  pvisor --stage /tmp/pvisor-cases/existing-stage -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-015`，命令为 `just cases --case S-DOC-015`。

- [ ] **C05：whole-rootfs 捕获与 tmpfs 隔离**

  建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

  用途：比较 workspace 写入和 sandbox 临时目录写入。前者用于保留任务改动，后者只供本次运行临时使用。

  准备：Linux user/mount namespace 可用；macOS Seatbelt 不提供此例要求的 whole-rootfs/tmpfs 隔离。

  预期：workspace 的改动出现在 stage，宿主 workspace 和宿主 `/tmp` 均不出现新文件。这里不验证 workspace 以外普通 rootfs 路径的持久化。

  ```bash
  pvisor --stage /tmp/pvisor-cases/root-stage -- /bin/sh -c \
    'printf workspace > ./workspace-change; printf tmp > /tmp/pvisor-root-change'
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-016`，命令为 `just cases --case S-DOC-016`。

- [ ] **C06：`--safe` 隔离 HOME 写入**

  用途：检查 `--safe` 除暂存 workspace 外，还为 HOME 提供独立的写时复制视图。示例把测试 HOME 放在专用目录，不触碰真实用户目录。

  准备：Linux user/mount namespace 可用。

  预期：Agent 能在自己的 HOME 中读回刚写入的状态；宿主 HOME 没有该文件，workspace 的持久 stage 仍可审查。

  ```bash
  mkdir -p /tmp/pvisor-cases/home
  HOME=/tmp/pvisor-cases/home pvisor --safe --stage /tmp/pvisor-cases/safe-home -- \
    /bin/sh -c 'printf private > "$HOME/state"; cat "$HOME/state"'
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-017`，命令为 `just cases --case S-DOC-017`。

### D. OverlayFS 与 Host 安全边界

D01 讲视图层组合，D02/D03 讲 host executor，D04–D06 讲拒绝、显式写入和交互授权。
文件规则为 `deny`、`ask`、`warn`：分别表示拒绝、询问、放行并警告；默认累加。
`--mount` 的 `read` 授予只读宿主共享（要求 host executor 加 `--safe`/`--ask`），
`stage` 组合写时复制底层，`write` 直接写入宿主 lower。
`--access PATH-GLOB:ask` 会自动启用审计 TUI 和 safe 暂存视图，无需另加 `--ask`。
文件弹窗的 `1` 仅允许此文件，`2` 允许同级目录中的文件，`3` 允许相同后缀的文件；
`d` 拒绝此目标。明确的 `deny` 规则仍直接拒绝，不弹窗。
弹窗默认仅对当前 session 生效；先按 `s`、`w`、`u`，分别选择 session、workspace、user，
再按数字选择授权范围，按 Enter 确认；默认选中拒绝按钮，`d` 直接拒绝。session 规则写入当前 Job 的 `audit-policy.json`；
workspace 和 user 规则写入 `~/.config/pvisor/config.toml` 的 `permissions` 部分
（设置了绝对路径 `XDG_CONFIG_HOME` 时使用该目录）。workspace 按规范化后的工作目录区分。
后续 TUI Job 加载这些规则，优先级为 session > workspace > user，同层最后匹配的规则生效。
持久化文件路径使用原始绝对路径；用户级后缀规则可覆盖其他工作区，授权时应注意范围。
可在 Permissions 面板查看规则，用 j/k 选择后按两次 x 移除决定；随后可能命中其他规则或重新询问。
`audit.jsonl` 记录人工及自动决策和保存范围。Job 记录默认保留；`--stage PATH` 可指定位置。

- [ ] **D01：高级 OverlayFS 组合**

  建议场景：适合检查 OverlayFS 视图和 host 安全边界。

  用途：把宿主的两个目录依次叠加到工作区视图，并指定 Agent 看到的路径。`directory` 选择目录后端；改动只通过显式 `apply` 提交。

  准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

  预期：记录的目标为 `view`，从顶层到底层依次为 `layer`、`base`、本次运行持有的 workspace 快照。目录为空，因此此例检查配置顺序，不检查同名文件覆盖内容。

  ```bash
  mkdir -p /tmp/pvisor-cases/base /tmp/pvisor-cases/layer "$PWD/view"
  pvisor \
    --stage /tmp/pvisor-cases/composed-stage \
    --mount "/tmp/pvisor-cases/base:$PWD/view:stage" \
    --mount "/tmp/pvisor-cases/layer:$PWD/view:stage" \
    -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-018`，命令为 `just cases --case S-DOC-018`。

- [ ] **D02：显式 host executor**

  建议场景：适合检查 OverlayFS 视图和 host 安全边界。

  用途：显式选择 host executor，观察当前系统上的隔离类型。

  准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

  预期：Linux 记录为 `rootless_process`，macOS 记录为 `sandboxed_process`；两者都不应降级为 host process。

  ```bash
  pvisor --executor host -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-019`，命令为 `just cases --case S-DOC-019`。

- [ ] **D03：host stage 隐藏原 workspace**

  建议场景：适合检查 OverlayFS 视图和 host 安全边界。

  用途：观察启用 stage 后子进程的 cwd 和 procfs 路径。三条命令的输出保存到 `views.txt`。

  准备：Linux user/mount namespace 可用；macOS 不支持此例的 procfs/mount namespace 路径隐藏语义。

  预期：cwd 指向 stage 的 merged 目录，输出中不出现原 workspace 路径。此例只检查路径显示，不证明所有原路径或继承 FD 访问都已被禁止。

  ```bash
  pvisor --stage /tmp/pvisor-cases/host-stage -- /bin/sh -c \
    'pwd; readlink /proc/self/root; readlink /proc/self/cwd' > views.txt
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-020`，命令为 `just cases --case S-DOC-020`。

- [ ] **D04：显式拒绝敏感路径读取**

  用途：用 `--access PATH-GLOB:deny` 阻止 Agent 在工作区中读取匹配的文件。

  准备：Linux user/mount namespace 可用。

  预期：读取失败并记录拒绝规则；宿主文件保持原样。

  ```bash
  mkdir -p private
  printf secret > private/token
  pvisor --stage /tmp/pvisor-cases/access-stage --access 'private/**:deny' -- \
    /bin/cat private/token
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-021`，命令为 `just cases --case S-DOC-021`。

- [ ] **D05：显式共享路径的直接写入**

  用途：用 `--mount SOURCE:write` 授予一个工作区之外的宿主目录可写访问。

  准备：Linux user/mount namespace 可用。

  预期：共享目录的 `out` 直接写入宿主 lower；无需对它执行 `pvisor apply`。

  ```bash
  mkdir -p /tmp/pvisor-cases/shared
  pvisor --mount /tmp/pvisor-cases/shared:write -- \
    /bin/sh -c 'printf mounted > "$1"' sh /tmp/pvisor-cases/shared/out
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-022`，命令为 `just cases --case S-DOC-022`。

- [ ] **D06：`ask` 弹窗与当前 Job 的目录授权**

  用途：用 `--access 'private/*.txt:ask'` 启动审计 TUI；第一次读取时按 `2`、Enter 授权同级目录，再读取另一文件，验证规则自动复用。

  准备：Linux user/mount namespace 和 Python 3 可用。示例用伪终端自动输入 `2`、Enter；手工运行时在弹窗中选择后按 Enter 确认。

  预期：只出现一次文件授权弹窗，两个文件均可读取；`audit-policy.json` 保存目录规则，`audit.jsonl` 记录第二次自动允许。`--stage` 保留当前 Job 的审计记录，不会把选择变成全局配置。

  ```bash
  mkdir -p private
  printf ASK_ONE > private/one.txt
  printf ASK_TWO > private/two.txt
  python3 - <<'PY'
  import fcntl, os, pty, select, signal, struct, time

  pid, master = pty.fork()
  if pid == 0:
      os.environ['TERM'] = 'xterm-256color'
      os.execvp('pvisor', [
          'pvisor', '--no-agent-defaults', '--stage', '/tmp/pvisor-cases/ask-stage',
          '--access', 'private/*.txt:ask', '--', '/bin/sh', '-c',
          'cat private/one.txt; sleep 1; cat private/two.txt',
      ])
  fcntl.ioctl(master, 0x5414, struct.pack('HHHH', 24, 100, 0, 0))
  screen = bytearray()
  prompted = False
  status = None
  deadline = time.monotonic() + 25
  try:
      while time.monotonic() < deadline:
          ready, _, _ = select.select([master], [], [], 0.1)
          if ready:
              try:
                  screen.extend(os.read(master, 65536))
              except OSError:
                  pass
          if not prompted and b'FILE ACCESS PAUSED' in screen:
              os.write(master, b'2\r')
              prompted = True
          ended, result = os.waitpid(pid, os.WNOHANG)
          if ended:
              status = result
              break
      if status is None:
          os.killpg(pid, signal.SIGTERM)
          _, status = os.waitpid(pid, 0)
          raise RuntimeError('timed out waiting for the Job')
  finally:
      os.close(master)
  assert prompted and os.waitstatus_to_exitcode(status) == 0
  assert b'ASK_ONE' in screen and b'ASK_TWO' in screen
  print('ASK directory grant reused')
  PY
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-023`，命令为 `just cases --case S-DOC-023`。

### E. VM 与 rootfs

需要更强边界、独立 guest kernel 或 OCI rootfs 时使用 VM。E01 最接近“直接运行”，E02/E03 展示目录和镜像来源，E04/E05 再加入资源与 stage。

- [ ] **E01：`--vm` 简写与 host rootfs**

  建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

  用途：用 `--vm` 选择 VM executor；Linux 默认以宿主根目录作为 guest rootfs。该方式扩大了 guest 可读取的宿主文件范围，只应在可信测试环境使用。

  准备：Linux；可访问 /dev/kvm。

  预期：guest 输出与宿主 workspace 相同的绝对路径。运行结果标记为虚拟机，网络使用 pVisor 的 smoltcp 驱动。

  ```bash
  pvisor --vm -- /bin/pwd > guest-cwd.txt
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-024`，命令为 `just cases --case S-DOC-024`。

- [ ] **E02：显式 VM executor 与目录 rootfs**

  建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

  用途：已有 Linux rootfs 时，直接把目录交给 VM 使用。目录内需要有可执行的 `/bin/pwd` 及其运行依赖。

  准备：Linux；可访问 /dev/kvm；准备好 Linux rootfs，并为脚本设置 PVISOR_CASE_ROOTFS。

  预期：虚拟机成功执行命令，guest cwd 与宿主 workspace 路径一致。

  ```bash
  pvisor --executor vm --rootfs /path/to/rootfs -- /bin/pwd > guest-cwd.txt
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-025`，命令为 `just cases --case S-DOC-025`。

- [ ] **E03：image rootfs**

  建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

  用途：使用 OCI 镜像准备 VM 的 rootfs，不依赖 Docker/Podman daemon。将 `image=` 后的占位符替换为可获取的镜像引用。

  准备：Linux；可访问 /dev/kvm；为脚本设置 PVISOR_CASE_IMAGE。

  预期：镜像准备后启动 VM，guest 的工作目录与宿主 workspace 路径一致。

  ```bash
  pvisor --vm --rootfs image=/path/to/image -- /bin/pwd > guest-cwd.txt
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-026`，命令为 `just cases --case S-DOC-026`。

- [ ] **E04：VM 资源配置**

  建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

  用途：使用已有 rootfs 启动 VM，同时指定 2GiB 内存和 2 个虚拟 CPU。Linux 静态构建已内嵌内核。

  准备：Linux；可访问 /dev/kvm；准备好 Linux rootfs，并为脚本设置 PVISOR_CASE_ROOTFS。

  预期：VM 成功运行，内存请求记录为 2147483648 字节。CPU 数量未由本例断言核验。

  ```bash
  pvisor --vm \
    --rootfs /path/to/rootfs \
    --memory 2GiB \
    --cpu 2 \
    -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-027`，命令为 `just cases --case S-DOC-027`。

- [ ] **E05：VM workspace 与 whole-rootfs stage 组合**

  建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

  用途：在 VM 镜像运行基础上增加持久 stage。工作区路径默认保持与宿主 cwd 一致。

  准备：Linux；可访问 /dev/kvm；为脚本设置 PVISOR_CASE_IMAGE。

  预期：guest cwd 保持一致，stage 保留在 `vm-stage`。本例只执行 `pwd`，验证路径和 stage 建立，不验证写入捕获。

  ```bash
  pvisor --vm \
    --rootfs image=/path/to/image \
    --stage /tmp/pvisor-cases/vm-stage \
    -- /bin/pwd > guest-cwd.txt
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-028`，命令为 `just cases --case S-DOC-028`。

- [ ] **E06：拒绝 executor 冲突**

  建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

  用途：验证互相冲突的 executor 参数不能一起使用：`--vm` 选择虚拟机，`--executor host` 却选择宿主。

  预期：参数归一化阶段失败，错误信息明确指出 `--vm` 与非 VM executor 冲突。

  ```bash
  pvisor --vm --executor host --rootfs host -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-029`，命令为 `just cases --case S-DOC-029`。

### F. Container

需要复用 OCI rootfs、但不想运行 Docker/Podman daemon 时使用原生 OCI container。请按 F01 → F04 逐步增加复杂度；F04 适合验证跨 ABI 注入和 mount 配置。

这些例子使用原生 OCI bundle，由 pVisor 准备文件系统并调用 runc/crun，
不依赖 Docker/Podman daemon。F01–F03 使用自动发现的 runtime，F04 显式选择 runc。
镜像或目录需与本机架构兼容；如果当前 pVisor 是动态链接构建，guest 必须提供
相应的动态加载器和库，否则应像 F04 一样指定兼容的静态构建。
默认注入当前 pVisor，不需要在最小命令中显式指定 binary。

- [ ] **F01：最小 container Job**

  建议场景：适合由 runc/crun 直接启动 OCI 容器的任务。

  用途：以 OCI 镜像启动最小容器运行。`--container-image` 同时选择容器 executor 和镜像来源。

  准备：OCI runtime 可运行，并为脚本设置 PVISOR_CASE_CONTAINER_IMAGE。

  预期：pVisor 准备 OCI bundle、注入自身并执行 `/bin/true`，运行结果记录为 container 且退出码为 0。

  ```bash
  pvisor --container-image alpine:latest -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-030`，命令为 `just cases --case S-DOC-030`。

- [ ] **F02：container rootfs 与隔离网络**

  建议场景：适合由 runc/crun 直接启动 OCI 容器的任务。

  用途：在 F01 基础上只增加 `--container-network none`，让容器使用独立的网络 namespace，不配置外部连接。

  准备：OCI runtime 可运行，并为脚本设置 PVISOR_CASE_CONTAINER_IMAGE。

  预期：容器中的 `/bin/true` 成功退出。此命令不发起网络请求，因此断言只检查启动成功，不验证网络是否能被绕过。

  ```bash
  pvisor --container-image alpine:latest \
    --container-network none \
    -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-031`，命令为 `just cases --case S-DOC-031`。

- [ ] **F03：使用宿主 rootfs 的 OCI bundle**

  建议场景：适合由 runc/crun 直接启动 OCI 容器的任务。

  用途：不提供容器镜像，直接以宿主 `/` 作为只读 lower。pVisor 会先建立独立 synthetic rootfs，再把宿主标准目录以只读方式映射进去。

  准备：Linux、可运行的 OCI runtime，以及允许 rootless container 的 user namespace。

  预期：以指定目录作为容器根文件系统执行命令，不需要配置镜像仓库。使用专用测试 rootfs，不要把宿主 `/` 当作此例的测试目录。

  ```bash
  pvisor --executor container \
    --rootfs host \
    --container-network none \
    -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-032`，命令为 `just cases --case S-DOC-032`。

- [ ] **F04：显式 OCI runtime 与高级 container 参数**

  建议场景：适合由 runc/crun 直接启动 OCI 容器的任务。

  用途：显式覆盖高级容器选项：使用 runc 和指定 pVisor 构建，声明平台、uid/gid、工作目录、只读 rootfs，并把宿主目录绑定到 `/workspace`。

  准备：OCI runtime 可运行，并为脚本设置 PVISOR_CASE_CONTAINER_IMAGE。

  预期：容器成功退出。`read_only=false` 使绑定目录可写，即使 rootfs 只读；`--container-workdir` 在 Run 没有 cwd 时才作为回退。此例仅检查组合启动，未分别检查用户身份和读写行为。

  ```bash
  pvisor --executor container \
    --container-runtime runc \
    --rootfs image=alpine:latest \
    --container-pvisor-binary ./target/release/pvisor \
    --container-platform linux/amd64 \
    --container-network none \
    --container-workdir /workspace \
    --container-user 1000:1000 \
    --container-read-only-rootfs \
    --container-mount 'source="/tmp/pvisor-cases/workspace",target="/workspace",read_only=false' \
    -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-033`，命令为 `just cases --case S-DOC-033`。

### G. OverlayNet

这一组只讨论网络边界。proxy 适合需要 host Gateway 的协作式访问，VM auto 和 host deny-all 才适合需要更强网络边界的场景。
使用 `--ask` 时，未列入规则的代理网络目标会暂停并弹窗：`1` 仅允许当前目标，
`2` 允许当前主机名及其子域名，范围仍限于相同端口和传输协议；IP 地址没有域名选项，
`d` 拒绝当前目标。与文件授权一样，先按 `s` / `w` / `u` 选择 session / workspace / user 保存范围。
显式拒绝规则不进入弹窗；未经代理的直接 socket 连接也不会触发此审计。

- [ ] **G01：启用默认 proxy**

  建议场景：适合配置出站网络、代理访问或禁止网络的任务。

  用途：只给出 `--overlaynet`，省略值时启用默认 proxy 模式。

  预期：记录为 explicit-proxy，拦截强度为 cooperative。它只约束经过代理的流量，不能据此认为直接 socket 已被禁止。

  ```bash
  pvisor --overlaynet -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-034`，命令为 `just cases --case S-DOC-034`。

- [ ] **G02：自定义 proxy 监听地址**

  建议场景：适合配置出站网络、代理访问或禁止网络的任务。

  用途：指定代理监听地址；该参数会自动启用 host proxy。手工运行时确保 18080 端口未被占用；脚本会替换为空闲端口。

  预期：记录的 OverlayNet 监听地址与请求一致，网络驱动为 explicit-proxy。

  ```bash
  pvisor --overlaynet-listen 127.0.0.1:18080 -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-035`，命令为 `just cases --case S-DOC-035`。

- [ ] **G03：allow、deny 和带宽限制组合**

  建议场景：适合配置出站网络、代理访问或禁止网络的任务。

  用途：组合配置允许目标、拒绝网段和针对目标的带宽上限。只有通过代理的流量才受这些规则约束。

  预期：记录为 allowlist 模式，允许 `api.example.com:443`，拒绝 `10.0.0.0/8`，并把 `1mbps` 记录为每秒 125000 字节。本例不实际发请求。

  ```bash
  pvisor --overlaynet-allow api.example.com:443 \
    --overlaynet-deny 10.0.0.0/8 \
    --overlaynet-limit api.example.com=1mbps \
    -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-036`，命令为 `just cases --case S-DOC-036`。

- [ ] **G04：deny-all 不可通过环境变量绕过**

  建议场景：适合配置出站网络、代理访问或禁止网络的任务。

  用途：验证禁止网络后，清除常见代理变量仍不能访问外网。Linux 使用 network namespace，macOS 使用 Seatbelt；需要宿主安装 curl，命令故意发起网络请求。

  准备：安装 curl。

  预期：命令失败，结果记录 no-network 和不可绕过边界。外网自身不可用也会使 curl 失败，因此本例不能单独证明隔离有效。

  ```bash
  pvisor --overlaynet-deny-all -- /bin/sh -c \
    'unset HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy; curl --max-time 2 https://example.com'
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-037`，命令为 `just cases --case S-DOC-037`。

- [ ] **G05：VM OverlayNet auto**

  建议场景：适合配置出站网络、代理访问或禁止网络的任务。

  用途：验证 VM 默认使用 OverlayNet auto，使流量经过虚拟机的 smoltcp 网络驱动。

  准备：Linux；可访问 /dev/kvm；准备好 Linux rootfs，并为脚本设置 PVISOR_CASE_ROOTFS。

  预期：记录为 vm-smoltcp 和 non-bypassable，而不是 host 的协作式代理。

  ```bash
  pvisor --vm --rootfs /path/to/rootfs -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-038`，命令为 `just cases --case S-DOC-038`。

- [ ] **G06：关闭 OverlayNet 时拒绝策略参数**

  建议场景：适合配置出站网络、代理访问或禁止网络的任务。

  用途：检查关闭 OverlayNet 后不能继续提供网络策略。

  预期：启动前失败，错误提示策略需要 `auto` 或 `proxy`。如果只想关闭 OverlayNet，请不要附带 allow/deny/limit。

  ```bash
  pvisor --overlaynet off --overlaynet-allow example.com:443 -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-039`，命令为 `just cases --case S-DOC-039`。

- [ ] **G07：审查被代理拒绝的具体目标**

  用途：通过 pVisor 注入的代理访问一个明确拒绝的域名，确认 Job 记录的是具体目标和拒绝次数，而不只是网络失败总数。请求在策略层被拒绝，不依赖该域名真实可访问。

  准备：安装 curl。

  预期：curl 请求失败；`status --review` 的 Network access observations 可指出 `blocked.example:80` 被拒绝。host proxy 是协作式边界，本例只证明经过代理的请求被拦截。

  ```bash
  pvisor --overlaynet-deny blocked.example -- /bin/sh -c \
    'curl --fail --silent --show-error --noproxy "" -x "$http_proxy" --max-time 2 http://blocked.example/'
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-040`，命令为 `just cases --case S-DOC-040`。

### H. Gateway 与记录

需要审计、模型路由或轨迹回放时使用这一组。H01 是 Gateway 配置示例，H02 把事件写成 JSONL。

- [ ] **H01：Gateway capture 完整组合**

  建议场景：适合接入 Gateway、模型路由或记录轨迹的任务。

  用途：为需要模型请求记录的 Agent 配置 Gateway。示例设置路由、管理监听端口、完整记录级别、会话头、诊断输出和 Markdown 投影，并保留 stage。

  准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

  预期：Gateway 与 stage 成功建立。示例上游是占位地址，`/bin/true` 不发送模型请求；此例不验证对话内容。管理监听地址与运行记录中的 `gateway_listen` 不是同一个服务地址。

  ```bash
  pvisor \
    --stage /tmp/pvisor-cases/gateway-stage \
    --gateway-mode capture \
    --gateway-admin-listen 127.0.0.1:19090 \
    --gateway-level full \
    --gateway-session-header X-Session-ID \
    --gateway-debug \
    --gateway-stream-markdown \
    --gateway-route 'name="default",upstream="https://example.com/v1"' \
    -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-041`，命令为 `just cases --case S-DOC-041`。

- [ ] **H02：JSON 记录**

  建议场景：适合接入 Gateway、模型路由或记录轨迹的任务。

  用途：把本次运行事件写成 JSONL 文件。

  预期：指定文件非空，首行具有 JSON 对象形式。每行应是一个事件；本例只检查文件建立和首行外观。

  ```bash
  pvisor --record-destination /tmp/pvisor-cases/events.jsonl -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-042`，命令为 `just cases --case S-DOC-042`。

### I. Spec 与控制面

已有自动化控制面或需要把 RunSpec 作为文件传递时使用这一组。I01 是 TOML 配置，I02 是 JSON 委托，I03 验证无扩展名文件。

- [ ] **I01：TOML config**

  建议场景：适合从 TOML/JSON 文件或控制面执行 RunSpec 的任务。

  用途：把命令写进 TOML 后通过 `--config` 运行。手工执行前创建 `pvisor.toml`，内容为 `[run]` 下的 `command = ["/bin/true"]`；semspec 夹具会预置此文件。

  预期：命令来自配置文件，无需在 CLI 重复；运行正常结束。

  ```bash
  pvisor --config ./pvisor.toml
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-043`，命令为 `just cases --case S-DOC-043`。

- [ ] **I02：JSON RunSpec**

  建议场景：适合从 TOML/JSON 文件或控制面执行 RunSpec 的任务。

  用途：执行已准备好的 JSON RunSpec，并把结果原子写入指定文件。手工运行前准备包含 run_id、agent 和 process invocation 的 `run-spec.json`；semspec 夹具预置的是运行 `/bin/true` 的 `case-i02`。

  准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

  预期：运行名称为 `case-i02`，`run-result.json` 非空。该委托路径当前只支持 host executor，不套用普通 Job 的 rootless safe profile；不要把此例视为隔离模式示例。

  ```bash
  pvisor --spec ./run-spec.json --result-file ./run-result.json --stage ./delegated-stage
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-044`，命令为 `just cases --case S-DOC-044`。

- [ ] **I03：无扩展名 spec**

  建议场景：适合从 TOML/JSON 文件或控制面执行 RunSpec 的任务。

  用途：验证配置的识别不依赖扩展名。手工执行时把 I01 的 TOML 内容保存成 `config-without-extension`；semspec 夹具会预置该文件。

  预期：`--config` 正常读取 TOML 并完成运行，不要求文件名以 `.toml` 结尾。

  ```bash
  pvisor --config ./config-without-extension
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-045`，命令为 `just cases --case S-DOC-045`。

### J. 复杂组合

这些是多项能力同时启用的回归示例：J01 偏 host 安全，J02 偏 VM，J03 偏容器。它们使用简短测试命令，不能代替真实 Agent 工作负载的验收；遇到问题时请拆回对应的 A–I 场景定位。

- [ ] **J01：host + persistent stage + deny-all + capture + limits**

  建议场景：适合上线前验证多项能力组合的端到端任务。

  用途：组合使用 host stage、禁止网络、输出采集、JSON 事件和资源限制。命令在隔离视图中写入一个结果文件。

  准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

  预期：原 workspace 不变；stage 记录 `result.txt`，结果保存 stdout 和资源请求，事件写入指定 JSONL 文件，网络标记为禁止连接。

  ```bash
  pvisor --name host-full \
    --stage /tmp/pvisor-cases/host-full \
    --overlaynet-deny-all \
    --stdio capture \
    --record-destination /tmp/pvisor-cases/host-full/trajectory/events.jsonl \
    --memory 512MiB \
    --max-processes 64 \
    --overlayfs-max-size 2GiB \
    --max-cpu-time 30s \
    -- /bin/sh -c 'pwd; printf changed > result.txt'
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-046`，命令为 `just cases --case S-DOC-046`。

- [ ] **J02：VM + image rootfs + stage + OverlayNet + Gateway**

  建议场景：适合上线前验证多项能力组合的端到端任务。

  用途：在 VM 中运行真实 Agent，同时保留 stage、使用 smoltcp 网络、Gateway 和 JSONL 轨迹记录。需提供含 Agent 及其依赖的镜像，并把示例上游替换为实际服务。

  准备：Linux；可访问 /dev/kvm；为脚本设置 PVISOR_CASE_IMAGE；PVISOR_CASE_AGENT 指向 guest 中也可执行的 Agent。

  预期：VM 以请求的内存运行，保留 stage 和轨迹目录。是否产生模型对话取决于 Agent 是否真的调用 Gateway；当前断言不检查对话内容。

  ```bash
  pvisor --name vm-full \
    --vm \
    --rootfs image=/path/to/image \
    --stage /tmp/pvisor-cases/vm-full \
    --overlaynet auto \
    --gateway-mode capture \
    --gateway-level dialogue \
    --gateway-route 'name="default",upstream="https://example.com/v1"' \
    --record-destination /tmp/pvisor-cases/vm-full/trajectory \
    --memory 4GiB \
    --cpu 4 \
    -- /usr/local/bin/agent
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-047`，命令为 `just cases --case S-DOC-047`。

- [ ] **J03：Container + stage + read-only root + no network**

  建议场景：适合上线前验证多项能力组合的端到端任务。

  用途：在容器中组合持久 stage、只读 rootfs、隔离网络和 stdout 采集。只读 rootfs 与可写 stage 是不同层面的设置。

  准备：OCI runtime 可运行，并为脚本设置 PVISOR_CASE_CONTAINER_IMAGE。

  预期：容器成功退出并留下 stage。命令为 `/bin/true`，不会生成文件变更或有意义的 stdout；本例不验证 stage 写入和网络阻断行为。

  ```bash
  pvisor --name container-full \
    --container-image alpine:latest \
    --container-read-only-rootfs \
    --container-network none \
    --stage /tmp/pvisor-cases/container-full \
    --stdio capture \
    -- /bin/true
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-048`，命令为 `just cases --case S-DOC-048`。

### K. Job 的审查与生命周期

Job 是面向用户的核心对象。以下命令都直接使用 Job 的 stage 路径作为 selector，无需额外的 `job` 子命令。

- [ ] **K01：审查并只读查看暂存文件**

  用途：运行后通过 `status --review` 查看变更，再用 `inspect` 读取暂存视图，并确认 inspect 无法写入。

  准备：Linux user/mount namespace 可用。

  预期：审查结果列出一个文件；只读视图能读到 `staged`，写入被拒绝，原 workspace 不变。

  ```bash
  pvisor --stage /tmp/pvisor-cases/review-stage -- /bin/sh -c 'printf staged > note.txt'
  pvisor status --review --json /tmp/pvisor-cases/review-stage > status.json
  pvisor inspect /tmp/pvisor-cases/review-stage -- /bin/cat note.txt
  if pvisor inspect /tmp/pvisor-cases/review-stage -- /bin/sh -c 'printf changed > note.txt'; then
    exit 1
  fi
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-049`，命令为 `just cases --case S-DOC-049`。

- [ ] **K02：选择性 apply 后丢弃剩余改动**

  用途：只提交 `one.txt`，保留 `two.txt` 在 stage 中等待决定，然后显式丢弃剩余改动。

  准备：Linux user/mount namespace 可用。

  预期：原 workspace 仅出现 `one.txt`；`two.txt` 从未进入 lower。

  ```bash
  pvisor --stage /tmp/pvisor-cases/partial-stage -- /bin/sh -c \
    'printf one > one.txt; printf two > two.txt'
  pvisor apply /tmp/pvisor-cases/partial-stage --path one.txt
  pvisor drop /tmp/pvisor-cases/partial-stage
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-050`，命令为 `just cases --case S-DOC-050`。

- [ ] **K03：从已停止 Job fork**

  用途：从源 Job 的暂存视图启动一个子 Job。子 Job 可以读取源改动，同时产生自己的独立变更。

  准备：Linux user/mount namespace 可用。

  预期：子 Job 读到 `inherited`，但两个 Job 的变更都没有直接写入原 workspace。

  ```bash
  pvisor --stage /tmp/pvisor-cases/source-stage -- /bin/sh -c 'printf inherited > inherited.txt'
  pvisor fork /tmp/pvisor-cases/source-stage -- /bin/sh -c \
    'cat inherited.txt; printf child > child.txt' > child.out
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-051`，命令为 `just cases --case S-DOC-051`。

- [ ] **K04：终止运行中的 Job**

  用途：让一个长时间运行的 Job 进入后台，然后按 stage 路径请求正常终止。

  准备：Linux user/mount namespace 可用。

  预期：Job 在睡眠结束前退出，`status --json` 报告 `cancelled`，不再处于 live 状态。

  ```bash
  pvisor --stage /tmp/pvisor-cases/live-stage -- /bin/sleep 30 > live.log 2>&1 &
  job_pid=$!
  for ((attempt=0; attempt<100; attempt++)); do
    test -f /tmp/pvisor-cases/live-stage/run.json && break
    sleep 0.05
  done
  pvisor kill /tmp/pvisor-cases/live-stage
  if wait "$job_pid"; then exit 1; fi
  pvisor status --json /tmp/pvisor-cases/live-stage > stopped.json
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-052`，命令为 `just cases --case S-DOC-052`。

### L. 可复用环境

`env` 有稳定名称和跨命令复用的可写层；它与一次性 Job 的 stage 生命周期不同。

- [ ] **L01：环境跨命令保留改动并可丢弃**

  用途：创建环境，执行写入，在下一次 `exec` 和只读 `inspect` 中查看同一份暂存状态，最后丢弃改动。

  准备：Linux user/mount namespace 可用。

  预期：两个后续命令都能读到 `staged`；原 workspace 始终没有 `env.txt`，丢弃后环境可继续使用。

  ```bash
  export PERSISTING_ENV_HOME=/tmp/pvisor-cases/envs
  pvisor env create dev
  pvisor env exec dev -- /bin/sh -c 'printf staged > env.txt'
  pvisor env exec dev -- /bin/cat env.txt
  pvisor env inspect dev -- /bin/cat env.txt
  pvisor env drop dev
  pvisor env status dev --json > env-status.json
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-053`，命令为 `just cases --case S-DOC-053`。

- [ ] **L02：环境提交后重置为空 stage**

  用途：通过 `env apply --all` 把环境的改动提交到 target，并保留可继续使用的环境。

  准备：Linux user/mount namespace 可用。

  预期：原 workspace 得到 `accepted.txt`；环境的新一代 stage 没有待提交文件。

  ```bash
  export PERSISTING_ENV_HOME=/tmp/pvisor-cases/envs
  pvisor env create dev
  pvisor env exec dev -- /bin/sh -c 'printf accepted > accepted.txt'
  pvisor env apply dev --all
  pvisor env status dev --json > env-status.json
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-054`，命令为 `just cases --case S-DOC-054`。

### M. Replay 与交互终端

- [ ] **M01：离线准备回放前缀**

  用途：用一个最小 mini-swe-agent 原生轨迹验证 `replay --prepare-only`。该模式解析前缀，不启动 Agent，也不执行历史工具。

  预期：输出结果的 phase 为 `prepared`，历史命令没有创建 `marker`。

  ```bash
  cat > trajectory.json <<'JSON'
  {"trajectory_format":"mini-swe-agent-1.1","info":{"mini_version":"2.4.6"},"messages":[{"role":"assistant","content":"historical action","extra":{"response":{},"actions":[{"tool_call_id":"call-1","command":"printf should-not-run > marker"}]}},{"role":"tool","content":"old observation","extra":{"returncode":0}}]}
  JSON
  pvisor replay --agent mini-swe-agent --trajectory ./trajectory.json \
    --after-step 1 --prepare-only \
    --state-dir /tmp/pvisor-cases/replay-state \
    --output-dir /tmp/pvisor-cases/replay-output > prepared.json
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-055`，命令为 `just cases --case S-DOC-055`。

- [ ] **M02：TUI 保留命令输出并可打开 Log 面板**

  用途：用伪终端执行 `--tui`，验证 Agent 输出、底栏引导键和 `Ctrl-]` → `l` 打开的浮动 Log 面板。交互终端由测试脚本提供。

  准备：Linux 和 Python 3。

  预期：子命令正常退出；屏幕流中出现命令输出、底栏引导和 Log 面板。

  ```bash
  python3 - <<'PY'
  import fcntl, os, pty, select, struct, subprocess, termios, time

  master, slave = pty.openpty()
  fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 100, 0, 0))
  env = os.environ.copy()
  env['TERM'] = 'xterm-256color'
  child = subprocess.Popen(
      ['pvisor', '--tui', '--', '/bin/sh', '-c', 'printf TUI_READY; sleep 1.5'],
      stdin=slave, stdout=slave, stderr=slave, env=env, start_new_session=True,
  )
  os.close(slave)
  screen = bytearray()
  sent = False
  deadline = time.monotonic() + 8
  try:
      while time.monotonic() < deadline:
          ready, _, _ = select.select([master], [], [], 0.1)
          if ready:
              try:
                  screen.extend(os.read(master, 65536))
              except OSError:
                  break
          if not sent and b'TUI_READY' in screen:
              os.write(master, b'\x1dl')
              sent = True
          if child.poll() is not None and not ready:
              break
      if child.poll() is None:
          child.kill()
      child.wait(timeout=2)
  finally:
      os.close(master)
  assert child.returncode == 0, child.returncode
  assert sent and b'TUI_READY' in screen
  assert b'Ctrl-]' in screen and b'pVisor Review' in screen
  print('TUI_READY status-bar log-panel')
  PY
  ```

  自动检查见 `tests/semantics/documented-cases.md` 中的规格 `S-DOC-056`，命令为 `just cases --case S-DOC-056`。
