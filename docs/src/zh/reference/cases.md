# pVisor Session 用户场景与回归示例

从最简单的 Session 开始，逐步加入资源限制、stage、VM、容器和网络功能，最后走完审查、分支与轨迹回放流程。
每个 case 先说明用途、准备和预期结果，再给出可执行命令；编号便于单独回归。
`run` 创建 Session；`status`、`inspect`、`apply`、`drop`、`fork`、`kill` 直接操作 Session。`replay` 从轨迹启动 Session。编号（如 A01）只用于回归报告和问题定位。



## 按需求选择

| 你的需求 | 建议先看 |
|---|---|
| 只想运行一个命令，或确认默认写入 | A01–A03、A07 |
| 需要超时、内存或文件限制 | A04、B01–B04 |
| 想保留、丢弃或检查文件改动 | C01–C06 |
| 需要组合 OverlayFS 层或授予路径权限 | D01、D04–D05 |
| 想了解 host 默认隔离 | D02–D03 |
| 使用 VM、宿主 rootfs 或 OCI 镜像 | E01–E06 |
| 使用原生 OCI 容器 | F01–F04 |
| 配置网络代理或禁止网络 | G01–G07 |
| 接入 Gateway 或记录轨迹 | H01–H02 |
| 从配置文件或 RunSpec 执行 | I01–I03 |
| 参考多能力组合 | J01–J03 |
| 审查、选择性提交、分支或终止 Session | K01–K04 |
| 准备轨迹回放 | M01 |

本文件保留用途、语义、违反示例和原 A01–M01 编号；每条场景链接到
`tests/spec/<feature>/S-ID.md` 中唯一的 Scrut 原生检查源码。`mise run cases`
按文件名选择规格，再调用 Scrut；不从文档页面提取检查。

## 如何使用

手工执行产品命令时，先准备一个测试工作目录，并确保 `pvisor` 在 `PATH` 中。
完整检查块包含 runner 提供的夹具和断言函数，请通过 `mise run cases` 执行。

```bash
mkdir -p /tmp/pvisor-cases/workspace
cd /tmp/pvisor-cases/workspace
```

从仓库根目录运行自动检查：

```bash
mise run cases -- --suite doc --list
mise run cases -- --subject-bin target/release/pvisor
mise run cases -- --case S-DOC-001,S-DOC-012 --keep
mise run cases
SUBJECT_BIN="$PWD/target/release/pvisor" mise run scrut -- test tests/spec/session/S-DOC-001.md
```

`mise run cases` 构建 release pVisor，发现本页的 52 个有效 DOC 规格和
[VM 控制与 RAM backing 的六条场景](cases-vm.md)，输出 JSON 报告到
`target/pvisor-case-report.json`。使用 S-DOC ID 选择 case；A01 对应 S-DOC-001，
C01 对应 S-DOC-012。标题保留完整的场景编号。也可以通过
`mise run cases -- --subject-bin PATH` 使用已有二进制。
L01、L02 和 S-DOC-053、S-DOC-054 随 `env` 功能移除；这些 ID 不再复用。

每条原生规格保留原来的命令、退出预期和全部断言。预期非零退出必须实际发生并通过原断言，
不作为 XFAIL。共享函数来自 `tests/spec/fixtures/`，读取当前 case 的 `run-bundle.json`、
`run.json` 和命令日志。配置、Session 数据和夹具都在独立临时 CASE_ROOT；失败保留现场，
`--keep` 也保留成功现场。缺少前提仍标记 SKIP，但返回失败，CI 不会把它算作通过。

这些规格与 fixture 仍待人工审核，检查成功不等于人工批准。历史台账保持不变；Scrut 的摘要
审核策略尚待维护者决定。`--require-reviewed` 当前明确拒绝执行，不能绕过审核要求。

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

## A. 基础调用与身份

这一组适合第一次使用 pVisor。先从 A01 开始；只有需要固定显示名、采集输出或显式传递环境变量时，再选择后续例子。

### S-DOC-001：A01 省略 `run` 的最简调用


建议场景：适合第一次使用 pVisor、确认命令和 Session 身份。

**语义**：输出当前 workspace 的绝对路径并成功退出。默认使用 host executor，不启用 stage；未配置网络策略时记录为 ambient。

**理由**：在当前目录执行一个命令，不需要显式写出 `run`。`--` 后全部是交给 Agent 的命令和参数。

**违反示例**：命令退出 0，但输出的是父目录，或默认启动了 stage。

运行与断言：[Scrut S-DOC-001](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-DOC-001.md)。

### S-DOC-002：A02 显式 `run` 与省略形式等价


建议场景：适合第一次使用 pVisor、确认命令和 Session 身份。

**语义**：两个输出文件内容相同，都是当前工作目录。两次运行会各自生成记录，Session ID 和时间可以不同。

**理由**：对比省略和显式写出 `run` 的两种调用。分别保存 Agent 的标准输出，便于比较。

**违反示例**：显式 run 与省略形式的工作目录输出不同。

运行与断言：[Scrut S-DOC-002](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-DOC-002.md)。

### S-DOC-003：A03 Session 名称和 stdio capture


建议场景：适合第一次使用 pVisor、确认命令和 Session 身份。

**语义**：Session 名称为 `smoke`，结果中的标准输出为 `hello`，未被截断。

**理由**：为这次运行命名，并将 Agent 输出保存到运行结果。`--name smoke` 指定显示名，`--stdio capture` 开启输出采集。

**违反示例**：Agent 名称丢失，或捕获的 hello 被标记为截断。

运行与断言：[Scrut S-DOC-003](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-DOC-003.md)。

### S-DOC-004：A04 超时


建议场景：适合第一次使用 pVisor、确认命令和 Session 身份。

**语义**：pVisor 非零退出，运行结果的失败类型为 `deadline_exceeded`。

**理由**：给运行设置墙钟超时。`100ms` 是从运行开始计时的持续时间，不是 CPU 时间；命令故意睡眠 10 秒。

**违反示例**：超时命令仍成功退出，或失败被记成可重试的普通 process_exit。

运行与断言：[Scrut S-DOC-004](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-DOC-004.md)。

### S-DOC-005：A05 严格执行模式拒绝 best-effort 边界


建议场景：适合第一次使用 pVisor、确认命令和 Session 身份。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：当前 host / container / VM 执行路径在启动 Agent 前均因缺少 Subprocess
  enforcement 证据而拒绝请求（`UnsupportedPolicy`）。此例验证 fail-closed，
  不代表 `--strict` 当前在任一 executor 上可达“更强沙箱已就绪”。

**理由**：要求严格执行能力检查。`--strict` 不接受所请求能力缺少强制执行证据；这里同时要求禁止网络。

**违反示例**：缺少请求能力的强制证据时仍启动 Agent，或拒绝时没有说明缺失的能力。

运行与断言：[Scrut S-DOC-005](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-DOC-005.md)。

### S-DOC-006：A06 显式环境投影


建议场景：适合第一次使用 pVisor、确认命令和 Session 身份。

**语义**：子进程可见 `TEST_PVISOR_VALUE=visible`；运行记录列出这个变量，但不声明整体继承宿主环境。

**理由**：只把指定的宿主环境变量传给子进程。变量仅为这条命令设置，通过 `--pass-env` 显式允许投影。

**违反示例**：显式允许的变量未传给 Agent，或记录错误声明继承了整个宿主环境。

运行与断言：[Scrut S-DOC-006](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-DOC-006.md)。

### S-DOC-007：A07 默认写入直接到 workspace


**语义**：命令退出后，`direct.txt` 直接出现在原 workspace，记录中没有 OverlayFS stage。

**理由**：验证普通 host Session 的默认可写 lower；无需为日常命令额外选择执行器或 stage。

**违反示例**：没有请求 stage 的 host 写入没有直接出现在工作区。

运行与断言：[Scrut S-DOC-007](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/session/S-DOC-007.md)。

## B. 资源限制

这一组展示“请求限制”和“实际强制”之间的区别。B01 用于查看完整配置，B02 才真正尝试触发文件大小限制。

### S-DOC-008：B01 组合使用所有资源限制


建议场景：适合需要控制或验证资源限制的任务。

**语义**：命令成功退出，五个请求值出现在运行记录中，同时报告生效值和限制机制。`/bin/true` 不消耗这些额度，此例不测试超限行为。

**理由**：组合设置内存、进程数、CPU 时间、打开文件数和单文件大小。`MiB` 是二进制单位；`--max-cpu-time` 与墙钟超时不同。

**违反示例**：限制参数被接受，但请求值记录错误，或缺少文件大小的生效值和 rlimit 机制。

运行与断言：[Scrut S-DOC-008](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/resources/S-DOC-008.md)。

### S-DOC-009：B02 文件大小限制实际生效


建议场景：适合需要控制或验证资源限制的任务。

**语义**：写入命令失败，落盘文件如果存在，其大小不超过 1024 字节；运行结果记录进程退出失败。

**理由**：验证单文件大小限制：将上限设为 1KiB，再尝试用 `dd` 写入 4KiB。

**违反示例**：dd 成功写出 4KiB，或失败后文件仍超过 1024 字节。

运行与断言：[Scrut S-DOC-009](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/resources/S-DOC-009.md)。

### S-DOC-010：B03 内存参数短别名


建议场景：适合需要控制或验证资源限制的任务。

**语义**：命令成功，记录中的请求值为 268435456 字节，与 `--memory 256MiB` 一致。

**理由**：使用 `--memory` 的别名 `--mem`，为一个简单命令设置 256MiB 内存额度。

**违反示例**：--mem 256MiB 被解析成不同于 --memory 的请求值。

运行与断言：[Scrut S-DOC-010](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/resources/S-DOC-010.md)。

### S-DOC-011：B04 Stage 总大小限制


建议场景：适合需要控制或验证资源限制的任务。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：stage 成功建立并保存在指定路径。此例只验证参数可用和目录建立；当前产物未记录该上限，也未在此例中尝试写满 stage。

**理由**：为持久 stage 请求 1GiB 的总大小限制。它限制的是 stage 总量，和 B02 的单个文件大小不是同一个概念。

**违反示例**：stage 命令成功退出，但文件系统状态未标为 staged，或存储路径不符。

运行与断言：[Scrut S-DOC-011](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/resources/S-DOC-011.md)。

## C. Stage 与 whole-rootfs

当你希望 Agent 可以自由修改文件、但不污染当前 workspace 时使用这一组。C01 是最常用的持久模式；C02 使用 `--safe` 自动选择并保留 stage，C03 演示对持久 stage 显式执行 `drop`。

### S-DOC-012：C01 持久 stage


建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：原 workspace 没有 `result.txt`；变更清单中出现该文件，指定 stage 内保留 `run-bundle.json`，便于之后查看。

**理由**：把本次运行的文件改动放进一个保留的 stage。命令在 workspace 里创建 `result.txt`。

**违反示例**：result.txt 穿透到原工作区，或变更清单遗漏它。

运行与断言：[Scrut S-DOC-012](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-012.md)。

### S-DOC-013：C02 默认保留 stage


建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：命令成功，日志中给出的存储目录及 Run Bundle 保留，原 workspace 没有新建的文件。

**理由**：无需手写存储路径。`--safe` 在没有指定 `--stage` 时使用持久 Session 存储，退出后保留改动。

**违反示例**：日志中的 Run Bundle 路径在退出后消失，或 result.txt 出现在原工作区。

运行与断言：[Scrut S-DOC-013](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-013.md)。

### S-DOC-014：C03 显式丢弃持久 stage 的改动


建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：stage 目录保留，运行记录的文件系统状态变为 `discarded`；原 workspace 没有新文件。

**理由**：指定持久 stage 路径，完成运行后通过 `pvisor drop` 显式丢弃其中的改动。

**违反示例**：drop 后记录仍标为 staged，或原工作区得到 result.txt。

运行与断言：[Scrut S-DOC-014](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-014.md)。

### S-DOC-015：C04 显式 stage 保留已有目录内容


建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

**语义**：命令成功，原有的 `user-file` 和新生成的 Run Bundle 都保存在指定目录。

**理由**：验证 `--stage PATH` 始终表示持久目录；目录里已有的用户文件不会因运行结束而被删除。

**违反示例**：运行清理删除了指定 stage 目录中原有的 user-file。

运行与断言：[Scrut S-DOC-015](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-015.md)。

### S-DOC-016：C05 whole-rootfs 捕获与 tmpfs 隔离


建议场景：适合隔离文件变更、保留 stage 或验证 whole-rootfs 的任务。

准备：Linux user/mount namespace 可用；macOS Seatbelt 不提供此例要求的 whole-rootfs/tmpfs 隔离。

**语义**：workspace 的改动出现在 stage，宿主 workspace 和宿主 `/tmp` 均不出现新文件。这里不验证 workspace 以外普通 rootfs 路径的持久化。

**理由**：比较 workspace 写入和 sandbox 临时目录写入。前者用于保留任务改动，后者只供本次运行临时使用。

**违反示例**：工作区写入未被暂存，或 sandbox 的 /tmp 写入出现在宿主 /tmp。

运行与断言：[Scrut S-DOC-016](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-016.md)。

### S-DOC-017：C06 `--safe` 隔离 HOME 写入


准备：Linux user/mount namespace 可用。

**语义**：Agent 能在自己的 HOME 中读回刚写入的状态；宿主 HOME 没有该文件，workspace 的持久 stage 仍可审查。

**理由**：检查 `--safe` 除暂存 workspace 外，还为 HOME 提供独立的写时复制视图。示例把测试 HOME 放在专用目录，不触碰真实用户目录。

**违反示例**：--safe 把 HOME/state 写到了宿主 HOME，或 Agent 不能读取自己的写入。

运行与断言：[Scrut S-DOC-017](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-017.md)。

## D. OverlayFS 与 Host 安全边界

D01 讲视图层组合，D02/D03 讲 host executor，D04–D05 讲拒绝和显式写入。
文件规则为 `deny`、`warn`：分别表示拒绝、放行并警告；默认累加。
`--mount` 的 `read` 授予只读宿主共享（要求 host executor 加 `--safe`），
`stage` 组合写时复制底层，`write` 直接写入宿主 lower。
执行后用 `review` 审查暂存改动，用 `apply` 接受选定路径，再用 `drop` 丢弃剩余改动。

### S-DOC-018：D01 高级 OverlayFS 组合


建议场景：适合检查 OverlayFS 视图和 host 安全边界。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：记录的目标为 `view`，从顶层到底层依次为 `layer`、`base`、本次运行持有的 workspace 快照。目录为空，因此此例检查配置顺序，不检查同名文件覆盖内容。

**理由**：把宿主的两个目录依次叠加到工作区视图，并指定 Agent 看到的路径。`directory` 选择目录后端；改动只通过显式 `apply` 提交。

**违反示例**：记录中 lower 的 layer/base 顺序颠倒，或 workspace 快照目录缺失。

运行与断言：[Scrut S-DOC-018](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-018.md)。

### S-DOC-019：D02 显式 host executor


建议场景：适合检查 OverlayFS 视图和 host 安全边界。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：Linux 记录为 `rootless_process`，macOS 记录为 `sandboxed_process`；两者都不应降级为 host process。

**理由**：显式选择 host executor，观察当前系统上的隔离类型。

**违反示例**：显式 host 请求降级为 host_process，而记录仍被当作隔离成功。

运行与断言：[Scrut S-DOC-019](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-019.md)。

### S-DOC-020：D03 host stage 隐藏原 workspace


建议场景：适合检查 OverlayFS 视图和 host 安全边界。

准备：Linux user/mount namespace 可用；macOS 不支持此例的 procfs/mount namespace 路径隐藏语义。

**语义**：cwd 指向 stage 的 merged 目录，输出中不出现原 workspace 路径。此例只检查路径显示，不证明所有原路径或继承 FD 访问都已被禁止。

**理由**：观察启用 stage 后子进程的 cwd 和 procfs 路径。三条命令的输出保存到 `views.txt`。

**违反示例**：启用 host stage 后 procfs 的 cwd 仍暴露原工作区路径。

运行与断言：[Scrut S-DOC-020](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-020.md)。

### S-DOC-021：D04 显式拒绝敏感路径读取


准备：Linux user/mount namespace 可用。

**语义**：读取失败并记录拒绝规则；宿主文件保持原样。

**理由**：用 `--access PATH-GLOB:deny` 阻止 Agent 在工作区中读取匹配的文件。

**违反示例**：private/token 被允许读取，或拒绝后审查记录没有该目标。

运行与断言：[Scrut S-DOC-021](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-021.md)。

### S-DOC-022：D05 显式共享路径的直接写入


准备：Linux user/mount namespace 可用。

**语义**：共享目录的 `out` 直接写入宿主 lower；无需对它执行 `pvisor apply`。

**理由**：用 `--mount SOURCE:write` 授予一个工作区之外的宿主目录可写访问。

**违反示例**：明确授予 write 的共享路径没有获得 mounted 内容。

运行与断言：[Scrut S-DOC-022](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-022.md)。

## E. VM 与 rootfs

需要更强边界、独立 guest kernel 或 OCI rootfs 时使用 VM。E01 最接近“直接运行”，E02/E03 展示目录和镜像来源，E04/E05 再加入资源与 stage。

### S-DOC-024：E01 `--vm` 简写与 host rootfs


建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

准备：Linux；可访问 /dev/kvm。

**语义**：guest 输出与宿主 workspace 相同的绝对路径。运行结果标记为虚拟机，网络使用 pVisor 的 smoltcp 驱动。

**理由**：用 `--vm` 选择 VM executor；Linux 默认以宿主根目录作为 guest rootfs。该方式扩大了 guest 可读取的宿主文件范围，只应在可信测试环境使用。

**违反示例**：VM 成功退出但 guest cwd 不同，或网络没有记录 vm-smoltcp 边界。

运行与断言：[Scrut S-DOC-024](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-024.md)。

### S-DOC-025：E02 显式 VM executor 与目录 rootfs


建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

准备：Linux；可访问 /dev/kvm；准备好 Linux rootfs，并为脚本设置 PVISOR_CASE_ROOTFS。

**语义**：虚拟机成功执行命令，guest cwd 与宿主 workspace 路径一致。

**理由**：已有 Linux rootfs 时，直接把目录交给 VM 使用。目录内需要有可执行的 `/bin/pwd` 及其运行依赖。

**违反示例**：目录 rootfs 的 VM 返回宿主工作区之外的 cwd。

运行与断言：[Scrut S-DOC-025](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-025.md)。

### S-DOC-026：E03 image rootfs


建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

准备：Linux；可访问 /dev/kvm；为脚本设置 PVISOR_CASE_IMAGE。

**语义**：镜像准备后启动 VM，guest 的工作目录与宿主 workspace 路径一致。

**理由**：使用 OCI 镜像准备 VM 的 rootfs，不依赖 Docker/Podman daemon。将 `image=` 后的占位符替换为可获取的镜像引用。

**违反示例**：镜像 VM 返回错误 cwd，或运行结果不标为 virtual_machine。

运行与断言：[Scrut S-DOC-026](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-026.md)。

### S-DOC-027：E04 VM 资源配置


建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

准备：Linux；可访问 /dev/kvm；准备好 Linux rootfs，并为脚本设置 PVISOR_CASE_ROOTFS。

**语义**：VM 成功运行，内存请求记录为 2147483648 字节。CPU 数量未由本例断言核验。

**理由**：使用已有 rootfs 启动 VM，同时指定 2GiB 内存和 2 个虚拟 CPU。Linux 静态构建已内嵌内核。

**违反示例**：2GiB 的内存请求在产物中被记成其他值。

运行与断言：[Scrut S-DOC-027](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-027.md)。

### S-DOC-028：E05 VM workspace 与 whole-rootfs stage 组合


建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

准备：Linux；可访问 /dev/kvm；为脚本设置 PVISOR_CASE_IMAGE。

**语义**：guest cwd 保持一致，stage 保留在 `vm-stage`。本例只执行 `pwd`，验证路径和 stage 建立，不验证写入捕获。

**理由**：在 VM 镜像运行基础上增加持久 stage。工作区路径默认保持与宿主 cwd 一致。

**违反示例**：VM stage 路径或 guest cwd 与请求不一致。

运行与断言：[Scrut S-DOC-028](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-028.md)。

### S-DOC-029：E06 拒绝 executor 冲突


建议场景：适合需要 VM guest kernel、独立 rootfs 或更强隔离的任务。

**语义**：参数归一化阶段失败，错误信息明确指出 `--vm` 与非 VM executor 冲突。

**理由**：验证互相冲突的 executor 参数不能一起使用：`--vm` 选择虚拟机，`--executor host` 却选择宿主。

**违反示例**：--vm 与 --executor host 的冲突被忽略并启动了工作负载。

运行与断言：[Scrut S-DOC-029](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-029.md)。

## F. Container

需要复用 OCI rootfs、但不想运行 Docker/Podman daemon 时使用原生 OCI container。请按 F01 → F04 逐步增加复杂度；F04 组合高级配置。

这些例子使用原生 OCI bundle，由 pVisor 准备文件系统并调用 runc/crun，
不依赖 Docker/Podman daemon。F01–F03 使用默认 runtime，F04 显式选择 runc。
镜像或目录需与本机架构兼容；如果当前 pVisor 是动态链接构建，guest 必须提供
相应的动态加载器和库，否则应像 F04 一样指定兼容的静态构建。
默认注入当前 pVisor，不需要在最小命令中显式指定 binary。

F04 显式设置 `--container-platform linux/amd64`，假设 Linux x86_64。此选项接受匹配的原生架构断言，拒绝跨架构值，即使提供了预制 rootfs；host/VM 配置会拒绝它。它不是模拟执行、程序自动发现或下载选择器。现有用例检查组合启动，不是平台拒绝矩阵。见[容器准备](../guides/executors/container.md)与[配置](config.md#settings)。

### S-DOC-030：F01 最小 container Session


建议场景：适合由 runc/crun 直接启动 OCI 容器的任务。

准备：OCI runtime 可运行，并为脚本设置 PVISOR_CASE_CONTAINER_IMAGE。

**语义**：pVisor 准备 OCI bundle、注入自身并执行 `/bin/true`，运行结果记录为 container 且退出码为 0。

**理由**：以 OCI 镜像启动最小容器运行。`--container-image` 同时选择容器 executor 和镜像来源。

**违反示例**：最小容器命令退出非零，或产物把 container 记为其他 executor。

运行与断言：[Scrut S-DOC-030](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/container/S-DOC-030.md)。

### S-DOC-031：F02 container rootfs 与隔离网络


建议场景：适合由 runc/crun 直接启动 OCI 容器的任务。

准备：OCI runtime 可运行，并为脚本设置 PVISOR_CASE_CONTAINER_IMAGE。

**语义**：容器中的 `/bin/true` 成功退出。此命令不发起网络请求，因此断言只检查启动成功，不验证网络是否能被绕过。

**理由**：在 F01 基础上只增加 `--container-network none`，让容器使用独立的网络 namespace，不配置外部连接。

**违反示例**：隔离网络配置使简单容器命令无法正常启动或完成。

运行与断言：[Scrut S-DOC-031](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/container/S-DOC-031.md)。

### S-DOC-032：F03 使用宿主 rootfs 的 OCI bundle


建议场景：适合由 runc/crun 直接启动 OCI 容器的任务。

准备：Linux、可运行的 OCI runtime，以及允许 rootless container 的 user namespace。

**语义**：以指定目录作为容器根文件系统执行命令，不需要配置镜像仓库。使用专用测试 rootfs，不要把宿主 `/` 当作此例的测试目录。

**理由**：不提供容器镜像，直接以宿主 `/` 作为只读 lower。pVisor 会先建立独立 synthetic rootfs，再把宿主标准目录以只读方式映射进去。

**违反示例**：宿主 rootfs 的 OCI bundle 没有作为 container 正常完成。

运行与断言：[Scrut S-DOC-032](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/container/S-DOC-032.md)。

### S-DOC-033：F04 显式 OCI runtime 与高级 container 参数


建议场景：适合由 runc/crun 直接启动 OCI 容器的任务。

准备：OCI runtime 可运行，并为脚本设置 PVISOR_CASE_CONTAINER_IMAGE。

**语义**：容器成功退出。`read_only=false` 使绑定目录可写，即使 rootfs 只读；`--container-workdir` 在 Run 没有 cwd 时才作为回退。此例仅检查组合启动，未分别检查用户身份和读写行为。

**理由**：显式覆盖高级容器选项：使用 runc 和指定 pVisor 构建，声明平台、uid/gid、工作目录、只读 rootfs，并把宿主目录绑定到 `/workspace`。

**违反示例**：高级 OCI 参数被接受但容器失败，或被错误记为其他 executor。

运行与断言：[Scrut S-DOC-033](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/container/S-DOC-033.md)。

## G. OverlayNet

这一组只讨论网络边界。proxy 适合需要 host Gateway 的协作式访问，VM auto 和 host deny-all 才适合需要更强网络边界的场景。
启动前配置静态网络规则；未经代理的直接 socket 连接可能绕过协作式代理。

### S-DOC-034：G01 启用默认 proxy


建议场景：适合配置出站网络、代理访问或禁止网络的任务。

**语义**：记录为 explicit-proxy，拦截强度为 cooperative。它只约束经过代理的流量，不能据此认为直接 socket 已被禁止。

**理由**：只给出 `--overlaynet`，省略值时启用默认 proxy 模式。

**违反示例**：默认 proxy 请求没有记录 explicit-proxy/cooperative，或 capture 产物缺失。

运行与断言：[Scrut S-DOC-034](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/network/S-DOC-034.md)。

### S-DOC-035：G02 自定义 proxy 监听地址


建议场景：适合配置出站网络、代理访问或禁止网络的任务。

**语义**：记录的 OverlayNet 监听地址与请求一致，网络驱动为 explicit-proxy。

**理由**：指定代理监听地址；该参数会自动启用 host proxy。手工运行时确保 18080 端口未被占用；脚本会替换为空闲端口。

**违反示例**：实际监听地址与所请求的 loopback 地址不一致。

运行与断言：[Scrut S-DOC-035](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/network/S-DOC-035.md)。

### S-DOC-036：G03 allow、deny 和带宽限制组合


建议场景：适合配置出站网络、代理访问或禁止网络的任务。

**语义**：记录为 allowlist 模式，允许 `api.example.com:443`，拒绝 `10.0.0.0/8`，并把 `1mbps` 记录为每秒 125000 字节。本例不实际发请求。

**理由**：组合配置允许目标、拒绝网段和针对目标的带宽上限。只有通过代理的流量才受这些规则约束。

**违反示例**：allow、deny 或带宽规则中的主机、端口、速率被丢失或改写。

运行与断言：[Scrut S-DOC-036](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/network/S-DOC-036.md)。

### S-DOC-037：G04 deny-all 不可通过环境变量绕过


建议场景：适合配置出站网络、代理访问或禁止网络的任务。

准备：安装 curl。

**语义**：命令失败，结果记录 no-network 和不可绕过边界。外网自身不可用也会使 curl 失败，因此本例不能单独证明隔离有效。

**理由**：验证禁止网络后，清除常见代理变量仍不能访问外网。Linux 使用 network namespace，macOS 使用 Seatbelt；需要宿主安装 curl，命令故意发起网络请求。

**违反示例**：清除代理环境变量后 curl 成功连到外网，或安全产物未声明强制边界。

运行与断言：[Scrut S-DOC-037](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/network/S-DOC-037.md)。

### S-DOC-038：G05 VM OverlayNet auto


建议场景：适合配置出站网络、代理访问或禁止网络的任务。

准备：Linux；可访问 /dev/kvm；准备好 Linux rootfs，并为脚本设置 PVISOR_CASE_ROOTFS。

**语义**：记录为 vm-smoltcp 和 non-bypassable，而不是 host 的协作式代理。

**理由**：验证 VM 默认使用 OverlayNet auto，使流量经过虚拟机的 smoltcp 网络驱动。

**违反示例**：VM 网络被记录为 cooperative proxy 而非 vm-smoltcp 强制边界。

运行与断言：[Scrut S-DOC-038](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/network/S-DOC-038.md)。

### S-DOC-039：G06 关闭 OverlayNet 时拒绝策略参数


建议场景：适合配置出站网络、代理访问或禁止网络的任务。

**语义**：启动前失败，错误提示策略需要 `auto` 或 `proxy`。如果只想关闭 OverlayNet，请不要附带 allow/deny/limit。

**理由**：检查关闭 OverlayNet 后不能继续提供网络策略。

**违反示例**：关闭 OverlayNet 后仍静默接受 allow 策略。

运行与断言：[Scrut S-DOC-039](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/network/S-DOC-039.md)。

### S-DOC-040：G07 审查被代理拒绝的具体目标


准备：安装 curl。

**语义**：curl 请求失败；`status --review` 的 Network access observations 可指出 `blocked.example:80` 被拒绝。host proxy 是协作式边界，本例只证明经过代理的请求被拦截。

**理由**：通过 pVisor 注入的代理访问一个明确拒绝的域名，确认 Session 记录的是具体目标和拒绝次数，而不只是网络失败总数。请求在策略层被拒绝，不依赖该域名真实可访问。

**违反示例**：请求失败，但审查记录没有 blocked.example:80 的具体拒绝目标和次数。

运行与断言：[Scrut S-DOC-040](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/network/S-DOC-040.md)。

## H. Gateway 与记录

需要审计、模型路由或轨迹回放时使用这一组。H01 是 Gateway 配置示例，H02 把事件写成 JSONL。

### S-DOC-041：H01 Gateway capture 完整组合


建议场景：适合接入 Gateway、模型路由或记录轨迹的任务。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：Gateway 与 stage 成功建立。示例上游是占位地址，`/bin/true` 不发送模型请求；此例不验证对话内容。管理监听地址与运行记录中的 `gateway_listen` 不是同一个服务地址。

**理由**：为需要模型请求记录的 Agent 配置 Gateway。示例设置路由、管理监听端口、完整记录级别、会话头、诊断输出和 Markdown 投影，并保留 stage。

**违反示例**：Gateway/stage 未建立，或 gateway_listen 不是有效的 loopback 地址。

运行与断言：[Scrut S-DOC-041](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/gateway/S-DOC-041.md)。

### S-DOC-042：H02 JSON 记录


建议场景：适合接入 Gateway、模型路由或记录轨迹的任务。

**语义**：指定文件非空，首行具有 JSON 对象形式。每行应是一个事件；本例只检查文件建立和首行外观。

**理由**：把本次运行事件写成 JSONL 文件。

**违反示例**：事件文件为空，或首行不是 JSON 对象。

运行与断言：[Scrut S-DOC-042](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/gateway/S-DOC-042.md)。

## I. Spec 与控制面

已有自动化控制面或需要把 RunSpec 作为文件传递时使用这一组。I01 是 TOML 配置，I02 是 JSON 委托，I03 验证无扩展名文件。

### S-DOC-043：I01 TOML config


建议场景：适合从 TOML/JSON 文件或控制面执行 RunSpec 的任务。

**语义**：命令来自配置文件，无需在 CLI 重复；运行正常结束。

**理由**：把命令写进 TOML 后通过 `--config` 运行。手工执行前创建 `pvisor.toml`，内容为 `[run]` 下的 `command = ["/bin/true"]`；脚本会预置此文件。

**违反示例**：配置文件中的命令被忽略，运行名称或终态不符。

运行与断言：[Scrut S-DOC-043](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/config/S-DOC-043.md)。

### S-DOC-044：I02 JSON RunSpec


建议场景：适合从 TOML/JSON 文件或控制面执行 RunSpec 的任务。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：运行名称为 `case-i02`，`run-result.json` 非空。该委托路径当前只支持 host executor，不套用普通 Session 的 rootless safe profile；不要把此例视为隔离模式示例。

**理由**：执行已准备好的 JSON RunSpec，并把结果原子写入指定文件。手工运行前准备包含 run_id、agent 和 process invocation 的 `run-spec.json`；脚本预置的是运行 `/bin/true` 的 `case-i02`。

**违反示例**：委托运行没有生成结果文件，或被误记成隔离的普通 Session。

运行与断言：[Scrut S-DOC-044](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/config/S-DOC-044.md)。

### S-DOC-045：I03 无扩展名 spec


建议场景：适合从 TOML/JSON 文件或控制面执行 RunSpec 的任务。

**语义**：`--config` 正常读取 TOML 并完成运行，不要求文件名以 `.toml` 结尾。

**理由**：验证配置的识别不依赖扩展名。手工执行时把 I01 的 TOML 内容保存成 `config-without-extension`；脚本会预置该文件。

**违反示例**：相同 TOML 因文件没有扩展名而不能执行。

运行与断言：[Scrut S-DOC-045](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/config/S-DOC-045.md)。

## J. 复杂组合

这些是多项能力同时启用的回归示例：J01 偏 host 安全，J02 偏 VM，J03 偏容器。它们使用简短测试命令，不能代替真实 Agent 工作负载的验收；遇到问题时请拆回对应的 A–I 场景定位。

### S-DOC-046：J01 host + persistent stage + deny-all + capture + limits


建议场景：适合上线前验证多项能力组合的端到端任务。

准备：Linux user/mount namespace 或 macOS Seatbelt 可用。

**语义**：原 workspace 不变；stage 记录 `result.txt`，结果保存 stdout 和资源请求，事件写入指定 JSONL 文件，网络标记为禁止连接。

**理由**：组合使用 host stage、禁止网络、输出采集、JSON 事件和资源限制。命令在隔离视图中写入一个结果文件。

**违反示例**：多能力组合让 result.txt 穿透工作区，或轨迹、资源、网络证据缺失。

运行与断言：[Scrut S-DOC-046](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-046.md)。

### S-DOC-047：J02 VM + image rootfs + stage + OverlayNet + Gateway


建议场景：适合上线前验证多项能力组合的端到端任务。

准备：Linux；可访问 /dev/kvm；为脚本设置 PVISOR_CASE_IMAGE；PVISOR_CASE_AGENT 指向 guest 中也可执行的 Agent。

**语义**：VM 以请求的内存运行，保留 stage 和轨迹目录。是否产生模型对话取决于 Agent 是否真的调用 Gateway；当前断言不检查对话内容。

**理由**：在 VM 中运行真实 Agent，同时保留 stage、使用 smoltcp 网络、Gateway 和 JSONL 轨迹记录。需提供含 Agent 及其依赖的镜像，并把示例上游替换为实际服务。

**违反示例**：组合 VM 没有保留 stage/trajectory，或丢失 4GiB 内存请求。

运行与断言：[Scrut S-DOC-047](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-047.md)。

### S-DOC-048：J03 Container + stage + read-only root + no network


建议场景：适合上线前验证多项能力组合的端到端任务。

准备：OCI runtime 可运行，并为脚本设置 PVISOR_CASE_CONTAINER_IMAGE。

**语义**：容器成功退出并留下 stage。命令为 `/bin/true`，不会生成文件变更或有意义的 stdout；本例不验证 stage 写入和网络阻断行为。

**理由**：在容器中组合持久 stage、只读 rootfs、隔离网络和 stdout 采集。只读 rootfs 与可写 stage 是不同层面的设置。

**违反示例**：组合 container 没有正常结束并保留 stage。

运行与断言：[Scrut S-DOC-048](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/container/S-DOC-048.md)。

## K. Session 的审查与生命周期

Session 是面向用户的核心对象。以下命令都直接使用 Session 的 stage 路径作为 selector，无需额外的 `session` 子命令。

### S-DOC-049：K01 审查并只读查看暂存文件


准备：Linux user/mount namespace 可用。

**语义**：审查结果列出一个文件；只读视图能读到 `staged`，写入被拒绝，原 workspace 不变。

**理由**：运行后通过 `status --review` 查看变更，再用 `inspect` 读取暂存视图，并确认 inspect 无法写入。

**违反示例**：inspect 可以写暂存视图，或原工作区出现 note.txt。

运行与断言：[Scrut S-DOC-049](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-049.md)。

### S-DOC-050：K02 选择性 apply 后丢弃剩余改动


准备：Linux user/mount namespace 可用。

**语义**：原 workspace 仅出现 `one.txt`；`two.txt` 从未进入 lower。

**理由**：只提交 `one.txt`，保留 `two.txt` 在 stage 中等待决定，然后显式丢弃剩余改动。

**违反示例**：仅选 one.txt 却把 two.txt 一并落地，或剩余 stage 未被丢弃。

运行与断言：[Scrut S-DOC-050](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-050.md)。

### S-DOC-051：K03 从已停止 Session fork


准备：Linux user/mount namespace 可用。

**语义**：子 Session 读到 `inherited`，但两个 Session 的变更都没有直接写入原 workspace。

**理由**：从源 Session 的暂存视图启动一个子 Session。子 Session 可以读取源改动，同时产生自己的独立变更。

**违反示例**：fork 读不到源 Session 变更，或子 Session 写入穿透原工作区。

运行与断言：[Scrut S-DOC-051](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/checkpoint/S-DOC-051.md)。

### S-DOC-052：K04 终止运行中的 Session


准备：Linux user/mount namespace 可用。

**语义**：Session 在睡眠结束前退出，`status --json` 报告 `cancelled`，不再处于 live 状态。

**理由**：让一个长时间运行的 Session 进入后台，然后按 stage 路径请求正常终止。

**违反示例**：kill 后 Session 继续 live，或终态不是 cancelled。

运行与断言：[Scrut S-DOC-052](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/stage/S-DOC-052.md)。

## L. 已移除的环境命令

`env` 已移除，对应规格 `S-DOC-053`、`S-DOC-054` 已登记为 retired，ID 不再复用。

## M. Replay 与交互终端

### S-DOC-055：M01 离线准备回放前缀


**语义**：输出结果的 phase 为 `prepared`，历史命令没有创建 `marker`。

**理由**：用一个最小 mini-swe-agent 原生轨迹验证 `replay --prepare-only`。该模式解析前缀，不启动 Agent，也不执行历史工具。

**违反示例**：prepare-only 执行了历史命令，创建 marker，或报告回放过工具调用。

运行与断言：[Scrut S-DOC-055](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/replay/S-DOC-055.md)。
