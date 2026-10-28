# 发布 PolicyVisor

当前稳定发布流程由 GitHub Actions 从版本 tag 构建原生组件，以 Python wheel
交付，并通过 Trusted Publishing 上传到 PyPI。安装方式见[安装指南](../start/installation.md)。
wheel 不包含 PyO3 扩展，也不使用 Maturin。

每个平台 wheel 标记为 `py3-none-<platform>`，并包含：

- Python `pvisor` 版本标记，不是启动器；
- 三个原生脚本 `pvisor`、`pvisor-tui`、`pvisor-replay`，直接安装到环境的 bin 目录；
- 独立构建的 `pvisor-daemon`，Linux x86_64 和 macOS ARM64 wheel 均包含；
- `libkrunfw.SOURCE` 来源记录；两个平台都内嵌压缩 guest 内核，不交付固件共享库。

发布集包含 Linux x86_64 和 Apple Silicon macOS wheel、原生 `.tar.gz` 归档、
对应固件源码、`SHA256SUMS` 和生成的 Homebrew 配方 `pvisor.rb`。
原生归档复用 wheel 中完全相同的载荷，不重新编译。
Python 源码分发包（sdist）不在发布集中。

## 一次性设置 {#one-time-setup}

1. 创建名为 `pypi` 的 GitHub environment。不要要求 reviewer；允许匹配 `v*`
   的 tag，以及用于渠道恢复的 `main` 分支。
2. 在 PyPI 发布设置中添加 pending Trusted Publisher：
   - PyPI project: `pvisor-sandbox`
   - GitHub owner: `DeepLink-org`
   - Repository: `pvisor`
   - Workflow: `release.yml`
   - Environment: `pypi`

3. 发布验收使用 GitHub 托管的 `ubuntu-latest` 和 `macos-latest` runner。
   Linux 准备固定工具链的 musl target 和 KVM 权限，执行真实 VM 验收；
   macOS 托管的 Apple Silicon runner 不支持嵌套虚拟化，因此执行安装 smoke
   检查。无需注册自托管 runner。

4. 如需自动发布 Homebrew，使用项目自有的 [`DeepLink-org/Formula`](https://github.com/DeepLink-org/Formula) tap。
   设置仓库变量 `HOMEBREW_TAP_REPOSITORY=DeepLink-org/Formula`，以及仅对此 tap
   拥有 Contents 写权限的 secret `HOMEBREW_TAP_TOKEN`。变量未配置时跳过 tap 更新，
   仍生成配方并运行 macOS Homebrew 安装测试。安装、命令定位及 `brew test` 通过后，
   工作流才提交 `Formula/pvisor.rb`，不注册或启动系统服务。

GitHub 中不存放 PyPI API token。pending publisher 可以在首次成功上传时
创建项目，但不预留名称。

首次推送发布 tag 前，确认 `pvisor-sandbox` 的 Trusted Publisher 与当前仓库、工作流和 environment 匹配。

## 准备一次发布

1. 在 `pyproject.toml`、`Cargo.toml` 的 workspace package 段以及
   `pvisor/__init__.py` 中更新同一 `X.Y.Z` 版本。
2. 刷新 lockfile 中的本地 workspace 版本，且不升级依赖：

   ```bash
   cargo metadata --format-version 1 >/dev/null
   ```

3. 提交版本变更并合并到 `main`。工作流会拒绝其 commit 无法从 `main` 到达
   的 tag。
4. 可选地手动运行 **Publish Release**，保留默认 `channel=build`，其余输入留空。它在 GitHub 托管 runner 上执行必需 CI
   和各平台验收、校验全部 wheel，
   并保留含归档、校验清单和配方的 `release-native` 制品，不对外发布。
5. 创建并推送匹配的稳定 tag：

   ```bash
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

## 恢复失败的发布 {#channel-recovery}

在 `main` 修复发布脚本后，从 `main` 新建一次 **Publish Release** 运行。
GitHub 的 **Re-run jobs** 仍使用原提交，无法带上修复。不要为了重试 Homebrew
或 PyPI 移动已发布的 tag，也不要重新构建同版本：新 wheel 的字节可能改变，
与已发布的不可变文件冲突。

填写 `release_tag=vX.Y.Z`，把原 tag 触发的 **Publish Release** 运行编号填入
`source_run_id`，再选择失败的渠道：

| `channel` | 执行内容 |
| --- | --- |
| `github` | 恢复已验收制品，补齐 GitHub Release 附件 |
| `pypi` | 恢复制品，检查／补齐 GitHub Release，再仅上传同版本缺失的 wheel |
| `homebrew` | 恢复制品，检查／补齐 GitHub Release，用当前脚本重新生成 tap 配方，测试安装并更新已配置的 tap |

恢复流程跳过完整 CI、编译和各平台 wheel 验收。原运行的 **Verify release
artifact set** 必须成功；该任务依赖完整的共享分发流水线。来源必须是本仓库
已结束的 tag 运行，且 tag 仍指向该运行的提交。整个运行可以因下游发布失败而
失败。手动 build-only、nightly 和 PR 运行不能作为恢复来源。

原 `release-wheels` 和 `release-native` 制品必须仍可下载，保留期为 30 天。
恢复时检查 wheel 版本与元数据、两个原生归档、两个固件源码归档、原配方，以及
`SHA256SUMS` 的全部条目。文件缺失、过期、混用或内容改变都会阻止发布。
GitHub 和 PyPI 重试仍核对现有文件哈希，不替换公开文件。每次恢复都先检查或
补齐 GitHub Release，再执行选定的下游渠道，保证 PyPI 上传前对应源码已公开。

Homebrew 恢复用 `main` 上的当前脚本生成单独的配方，保留原归档 URL 和
SHA-256，并仅把实际通过安装与 host 执行测试的配方提交到 tap。原 Release
附件、校验清单和二进制载荷均保持不变。二进制修复或构建／验收失败需要重新
验证的新版本，不能通过此恢复入口跳过门禁。

## 构建与校验路径

PEP 517 backend 是 setuptools，配合仓库自有的
`scripts/packaging/build_backend.py`。组装 wheel 之前，它会构建 `pvisor` Rust
CLI 等原生组件并暂存平台载荷。`setup.py` 直接安装二进制脚本，不使用 Python console-entry-point 包装器，并把 wheel 标为
平台相关，同时把 Python 与 ABI tag 保持为 `py3-none`。

打包使用 `fw/` 中固定版本的固件源码，校验下载的内核输入，并按输入缓存构建。
`PVISOR_LIBKRUNFW_PATH` 可选择已有 payload。缺少固件是构建错误。各平台导出
对应固件源码，与 wheel 一起发布。

Linux CLI 使用 `x86_64-unknown-linux-musl` 全静态链接，并内嵌 libkrunfw 内核，
wheel 不包含固件共享库。wheel 保留 manylinux_2_28 标签以支持 glibc Python
安装器。构建需要 Zig、cargo-zigbuild 和 Rust musl target；固件只在构建时加载。

Apple Silicon macOS 使用原生 Darwin linker 构建 CLI，签署 HVF entitlement，
并与 Linux 一样内嵌压缩 guest 内核。构建出来的固件 dylib 仅在编译阶段读取。两个平台都内嵌静态 Linux musl Rust guest，
由 `rust-lld` 自动构建。macOS 需要 `aarch64-unknown-linux-musl` Rust stdlib，
构建 guest 不需要 Zig。

稳定版和 nightly 调用同一份 `distribution.yml`：在选定提交上执行必需 CI、
构建 wheel、平台验收、完整发布集校验，以及原生归档和校验清单生成。
发布任务下载这批已验证制品，不重新构建。每个最终 wheel 在对应
GitHub 托管 runner 的全新环境中安装。Linux 验收覆盖 host 退出码、strict host 执行拒绝、真实
VM 启动、暂停／冷恢复后内存进度继续、Job 身份保持且 Attempt 更换、文件暂存、
review。另一条新 VM Job 验证选择性 apply；恢复后的叠层 Job 必须拒绝当前不支持
的 apply。小型静态 guest 由仓库源码编译，缺少 KVM 权限会导致 Linux 验收失败。
macOS 校验原生载荷、签名 entitlement、安装、版本和命令转发；托管 runner
不执行真实 VM 验收。`release-acceptance-*` 或 `nightly-acceptance-*` 制品保存 Linux 的 wheel 与安装后
二进制 SHA-256 及命令结果，以及 macOS 的安装 smoke 输出。在任一受支持的
原生宿主机上运行完整验收：`mise run accept-wheel -- WHEEL LOG_JSON`；
macOS VM 验收仍需独立的硬件验证记录。

发布集检查要求每个平台恰好一个受支持 wheel、版本匹配、包元数据有效，以及产物
大小有界。两个平台的验收与完整发布集均通过后，才能向任一渠道发布。

构建 runner 还通过 Homebrew 式软链接布局测试原生归档，包括 host 执行和 companion
定位。归档原样复制已签名的 macOS 程序；guest 内核已内嵌，来源记录保留在可执行文件旁。稳定发布的
Homebrew 任务在 macOS 下载并安装已发布的归档，移除 runner 的 Cellar 组写和其他用户写权限，
随后运行 `brew test`，在 Homebrew 沙箱内检查版本、协议和 companion 帮助。
任务再从沙箱外执行真实 host Job，检查退出码和 `hello` 输出，卸载配方后才更新 tap。
Homebrew 的 Seatbelt 沙箱不能嵌套 pVisor 的沙箱；目录权限要求见[安装指南](../start/installation.md)。
Linux Homebrew 安装及升级／回退仍缺少原生验收。

稳定版本不可变。重跑在发布前核对 GitHub/PyPI 现有文件的 SHA-256；相同内容
保留，缺失文件可以补齐，同文件名内容不同则失败，必须发布新版本。GitHub 上传
不覆盖已有资源，PyPI 不再仅凭文件名跳过。先在 GitHub 发布对应固件源码，再向
PyPI 发布 wheel。配置了门禁不等于验收已经通过。

Linux x86_64 daemon 使用原生 `pvisor-vm`，需要 KVM 和委派 cgroup v2。新模板
使用共享 Rust guest 引擎与宿主协议适配，显式提供 execd/envd 时保留上游后端。
wheel 验收覆盖原生 Job；daemon SDK 兼容性、containerd 集成和密度仍需各自的
验收证据。见 [daemon 准备](../guides/daemon/index.md)。

## Nightly 构建

**Nightly Build** 每天 UTC 03:00（北京时间 11:00）运行，也可以在 `main` 上手动
触发；普通 push 运行 CI，不构建 nightly wheel。Nightly 与稳定发布共用
同一份发布准备流水线。nightly 先校验源码版本和 Cargo lockfile，再执行完整 CI、
双平台构建与验收，包括 Linux 安装后的真实 VM 验收。任一步失败均阻止更新滚动
nightly。这会在 main 上提前验证发布准备链路，但不保证未来 tag、PyPI 权限或
Homebrew 发布一定成功。

Nightly 版本追加 `+g<run-number>.<commit>`，仅更新 GitHub 的 `nightly` release。
稳定 tag 先把已校验 wheel、原生归档、校验清单、配方和对应固件源码上传到 GitHub Release，再仅将 wheel 上传到 PyPI。Nightly 发布已验证的原生归档、校验清单、wheel 和源码，但不生成 Homebrew 配方或更新 tap。daemon 可从两个平台的 nightly wheel 安装或从源码构建。缓存服务支持两个平台；sandbox 服务仍需上述 Linux 运行条件。不发布 daemon 独立归档包。

## 分发演进 {#distribution-plan}

发布流程已从校验过的 wheel 派生原生下载包和 Homebrew 配方。
可选版本的独立安装器、结构化发行清单及其他包管理渠道仍属提案。

### 渠道定位

| 渠道 | 面向用户 | 当前状态与方向 |
| --- | --- | --- |
| PyPI | Python 环境中的 CLI 用户 | 已有 wheel 流程；补充独立工具环境的安装、升级验收 |
| GitHub Release | CI、服务器与离线安装 | 已有 wheel、原生归档、固件源码及 SHA-256 清单；可选版本的安装器仍属提案 |
| Homebrew | Apple Silicon macOS 与 Linux x86_64 用户 | 生成配方并测试 macOS 安装；自动更新自有 tap 需要上述仓库变量与 token |
| npm | Node 工具链中的 CLI 用户 | 按需求增加原生程序安装入口；与 TypeScript SDK 分开交付 |
| crates.io / Cargo | Rust 嵌入与源码构建用户 | 后续评估；先处理内部 path 依赖、构建资源和公开 API 的发布边界 |

### 共享原生产物

每个平台完成原生构建后，wheel、二进制归档和包管理器入口复用同一组组件。
发行清单拟记录版本、commit、平台、组件哈希及固件来源；各归档分别记录自己的
SHA-256。发布阶段上传已验收的产物，不再次编译。同版本的 CLI、companion、
daemon 与固件作为一个安装单元升级，避免部分更新造成不兼容。

最终布局 smoke test 覆盖原生归档软链接和 Python 环境中的 companion 定位；
稳定发布还在 macOS 执行 Homebrew 安装测试。安装器应支持指定版本、用户级目录、校验、升级、回退和
卸载；卸载保留 Job 与用户缓存。包管理器安装的程序由该包管理器升级。
安装 daemon 可执行文件与注册、启动系统服务分开进行。

首批继续覆盖 Linux x86_64 与 macOS arm64。扩大平台支持前，增加相应原生验收。
渠道验收除版本输出外，还需覆盖首次有效执行、已有暂存审查流程、升级和卸载；
VM 能力沿用真实硬件验收。Stable 保持不可变，nightly 使用独立入口。

### CLI、协议与 SDK

运行现有 Agent CLI 不要求语言 SDK：pVisor 直接启动命令，Agent 的实现语言不影响
这一入口。平台集成可先使用已文档化的命令、退出码和
[JSON 输出](../reference/json-output.md)，并按[兼容边界](../reference/stability.md)
固定版本；完整 schema 和跨版本稳定协议仍需完善。

先收敛启动、取消、状态、事件与文件审查的结构化契约，再根据实际集成用户选择
TypeScript 或 Python SDK。npm 安装入口可以只分发 Rust 原生 CLI；它不要求
TypeScript 重写，也不意味着已经提供 SDK。SDK 不是当前 CLI 发布的前置条件。
