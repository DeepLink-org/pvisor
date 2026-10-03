# 完整环境快照 CLI

Linux x86_64（KVM）和 macOS Apple Silicon 上可通过 `pvisor snapshot` 保存和恢复完整 VM 环境。快照使用独立实例：CPU、RAM、设备和完整文件副本在同一次冻结中保存，发布完成后原 runner 退出；恢复创建新的工作副本并继续原进程。

## 使用方法

准备一个解包的 Linux rootfs 目录。在终端 A 中启动工作负载：

```sh
pvisor snapshot run --name task-a --rootfs /path/to/linux-rootfs -- bash
```

默认配置是 2 vCPU、256 MiB；可用 `--cpus` 和 `--memory` 设置。`--ram-storage raw` 为默认的完整 RAM 文件基线，`--ram-storage compressed` 将 RAM 按 64 KiB 分块、压缩并在快照存储中去重。命令参数放在 `--` 后，参数中的空格会保留。文件系统首先复制到实例私有目录，工作负载不会修改输入 rootfs。固件使用 pVisor 已有的校验下载和缓存机制。

在终端 B 中保存：

```sh
pvisor snapshot save task-a
```

命令输出快照 ID。发布成功后终端 A 的 runner 退出。如果请求端在发布后断开连接，快照仍已保存，原实例仍退出；用 `list` 找到对象。

恢复原状态：

```sh
pvisor snapshot restore SNAPSHOT_ID --name task-b
```

恢复继续原 guest、进程和打开文件。实例名称由 1–48 个 ASCII 字母、数字、下划线或连字符组成；名称单次使用，恢复时使用新名称。

```sh
pvisor snapshot list
pvisor snapshot delete SNAPSHOT_ID
pvisor snapshot gc
```

删除会拒绝仍有读取引用的对象。已安装全部状态并取得私有工作副本的运行实例不再依赖发布对象，删除不会损坏该实例。`gc` 清理崩溃留下的未发布写入和删除暂存，不删除发布对象，也不会清理活跃写入者。

全部命令均可加 `--store /path/to/store`；不同终端须使用相同目录。默认目录为系统用户数据目录下的 `pvisor/snapshots`，macOS 通常是 `~/Library/Application Support/pvisor/snapshots`。控制 socket 位于同 UID 拥有的私有短路径目录，以避免 macOS 的 Unix socket 路径长度限制。

## 持久化压缩与分叉

启动时选择持久化压缩：

```sh
pvisor snapshot run --name task-a --rootfs /path/to/linux-rootfs \
  --ram-storage compressed -- bash
pvisor snapshot save task-a
```

压缩路径复用冷页池的内容身份与 Fill / Zstd / Raw 编码。完整环境 manifest v2 记录 RAM 块顺序、长度和整体摘要；内容放在 `STORE/content/ID`，对象内部的硬链接持久持有引用。同一快照重复块及不同快照相同块共享编码对象，发布不依赖存活的池进程。发布前持久同步内容和引用，只有整个环境目录原子发布后才能恢复。新原始 RAM 快照使用 manifest v3，保存每个 64 KiB 块的 SHA-256 摘要；读取格式 v1 的兼容路径保留。

`delete` 移除环境引用；运行中的 VM 通过打开的原始 RAM 文件或活跃暂存硬链接保留后续读取所需的内容。`gc` 仅回收没有快照或活跃暂存引用的内容，并清理退出进程留下的暂存引用。恢复前检查 manifest、机器状态、文件树和 RAM 索引，读取 RAM 块时检查内容摘要；读取失败终止恢复或 VM，禁止用零填充替代损坏内容。内容摘要不作为来自不可信用户的认证。

同一对象可恢复为多个独立 VM。`fork` 是 `restore` 的可见别名，**从已保存对象分叉**；先保存源实例，再分别在两个终端运行：

```sh
pvisor snapshot fork SNAPSHOT_ID --name branch-a
pvisor snapshot fork SNAPSHOT_ID --name branch-b
```

两个 VM 从同一保存点继续，guest 内 boot ID/PID 保持保存时的值，宿主实例名称、runner 和工作目录不同。每个实例取得完整私有 RAM 和文件副本，堆、文件修改和打开句柄互不影响；安装完成后不依赖发布对象。当前不是保留源 VM 运行的 live fork，也没有低成本共享运行态 RAM 或文件 COW。

恢复通过只读 FUSE RAM 文件和 `MAP_PRIVATE` 映射按需加载：guest 或设备首次访问时读取并校验对应块，压缩块按需解码，guest 写入使用 COW 私有页，不修改快照或其他分支。启动前不再全量解码、写临时 RAM 文件或复制全部 RAM；v1 原始快照仍先检查整体摘要，再按需映射。Linux 需要可用的 `/dev/fuse` 和挂载权限，macOS 需要 macFUSE 内核后端。每个 runner 持有自己的 RAM 挂载和有界块缓存，尚未共享跨 runner 的解码页缓存。独立的退出监视进程在 runner 退出后卸载 RAM，包括绕过析构函数的退出和 `SIGKILL`。普通 RAM offload 禁止丢弃恢复后的 COW 页；保存新的完整快照仍会读取全部 RAM。文件树仍为完整副本；活跃冷页 pager 的直接保存尚未接入，恢复延迟和物理内存收益需要实测。

## 首版边界

- `snapshot save` 保存由 `snapshot run` 或 `snapshot restore` 启动的实例。普通 `pvisor run` 的 Job、Overlay/DAX、Gateway 和冷页池尚未接入该保存入口。
- 当前要求同宿主、同一次宿主启动、相同 CLI 构建和固件。升级 CLI 后旧快照会因构建身份不同而拒绝恢复。
- 输入是完整目录，当前没有 `image=...` 镜像解析；网络、额外宿主挂载、活跃外部连接、writeback、unlink-open、嵌套 VM 和冷页 pager 不支持。首版未提供网络设备或宿主网络透传。
- 树外硬链接、socket、FIFO、设备文件和非零 BSD 文件标志会明确拒绝。源目录应由用户独占，不能有外部宿主写入者。
- 保存仍捕获完整 RAM 并复制完整文件树；压缩模式仅改变 RAM 的持久编码与共享。它没有增量、懒加载、低成本 VM 分叉或物理密度收益承诺。
- VM 的工作副本留在 `STORE/runs/NAME/rootfs`；`gc` 只负责快照暂存，不自动删除工作副本。停止实例后可自行归档或清理该目录。

自带 `/init.krun` 的原生 Linux 根目录可使用 `snapshot run --native-init`，此时无需指定工作负载命令。

## 验证

`scripts/check-snapshot-cli.py --ram-storage compressed --fork` 使用产品二进制和普通 guest launcher 验证 run/save/fork/list/delete/gc：原参数、boot ID/PID、后台线程、32 MiB 堆和打开文件连续；保存后删除输入及原私有目录，恢复后删除发布对象，原任务仍完成校验。两个并发分支分别修改堆与已打开文件，先修改的分支不影响另一分支；删除环境及回收持久块后两者仍保持运行。`--ram-storage raw --fork` 验证原始格式基线。该实验是正确性验收，不是性能基准。
