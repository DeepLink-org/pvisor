# VM 暂停、恢复与文件 RAM 场景

这些 DOC 规格补充 [现有场景](cases.md)，支持 Linux/KVM 和 Apple Silicon macOS/HVF。
这些规格仍待人工审核；执行成功不等于批准。使用的 SDK 驱动源码是
`crates/pvisor/examples/vm_control_case.rs`，应连同规格一起审查。

`mise run cases` 发现链接的原生规格；SDK 驱动未准备时，四条 SDK 场景明确报告 SKIP 并使门禁失败。
`mise run cases -- --suite vm` 构建并在 macOS 签名驱动，再执行本页的全部六条场景。
也可以对已有二进制运行：

```sh
mise run cases -- --suite vm --subject-bin target/release/pvisor
```

macOS 必须设置 `PVISOR_CASE_ROOTFS` 为 Linux guest rootfs；Linux 默认使用 `/`。
SDK 场景需要 guest `/usr/bin/python3`，可用 `PVISOR_CASE_VM_PYTHON` 改路径。
驱动和固件目录可分别通过 `PVISOR_CASE_VM_DRIVER`、`PVISOR_CASE_VM_LIBRARY_DIR` 指定。
多 vCPU guest 分配并校验 8 MiB 固定数据和持续修改的 1 MiB 页面内容，通过测试专用 stage upper 的 heartbeat/release
文件与驱动握手；这只是测试夹具，不是产品控制面。检查暂停期间 heartbeat 停止、
恢复后推进、重复回收后 SHA-256 不变。不要求驻留页数归零。

### S-DOC-057：N01 启动指定 RAM 文件，退出后保留


**语义**：指定新的 RAM 文件后，guest 成功执行；文件权限为 0600，包含非空 RAM backing，正常退出后仍存在。

**违反示例**：文件未被创建、权限向其他用户开放，或正常退出删除了调用者指定的文件。

运行与断言：[Scrut S-DOC-057](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-057.md)。

### S-DOC-058：N02 已有 RAM 文件不能被启动覆盖


**语义**：启动时指定已有文件必须失败，文件内容保持，guest 命令不执行。

**违反示例**：截断已有文件以建立 RAM backing，或者报错后仍执行 guest。

运行与断言：[Scrut S-DOC-058](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-058.md)。

### S-DOC-059：N03 pause/resume 幂等，使用启动 backing 回收


**语义**：重复 pause 停止 guest 推进；重复 resume 继续执行；沿用启动文件的两次 offload 保持 guest 内存完整，暂停/恢复状态与控制完成事件一致。

**违反示例**：暂停返回成功但 heartbeat 继续变化、恢复不推进，或第二次回收后 guest 内存校验失败。

运行与断言：[Scrut S-DOC-059](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-059.md)。

### S-DOC-060：N04 offload 发布新文件并保持 live inode


**语义**：offload 选定同一文件系统中的新路径，文件与启动 backing 属于同一 inode；结果路径和 RAM 范围正确，恢复后 guest 内存完整，重复 offload 保持文件选择。

**违反示例**：复制到了另一个 inode、回报错误的路径、回收后 guest 内存损坏，或下次 offload 悄悄换回原文件。

运行与断言：[Scrut S-DOC-060](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-060.md)。

### S-DOC-061：N05 offload 拒绝覆盖，VM 继续运行


**语义**：offload 选定已有文件时拒绝操作，已有内容、Running 状态和未取消状态保持；guest 继续推进，之后 pause/resume 仍可完成。

**违反示例**：拒绝路径后截断目标、暂停或取消了 VM，或使后续命令的确认串线。

运行与断言：[Scrut S-DOC-061](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-061.md)。

### S-DOC-062：N06 Seekable 增量 backing 回收与恢复


**语义**：启动时启用压缩，offload 返回合法 PVZRAM v2 manifest 与不可变 base/delta bundle；逻辑 RAM 大小完整，每次 offload 后整个 bundle 实际分配的磁盘空间均小于该测试负载的 RAM 范围。每次恢复后修改产生不同的 generation，其链可读取原文；十次回收、恢复覆盖从八层到一层的 compaction，不可达 generation 被回收，最多保留八层。guest 的固定数据 SHA-256 和持续更新的 1 MiB 页面内容在全部循环后均保持，暂停/恢复事件与状态一致。generation 不宣称跨设备的原子快照。

Linux 需要 FUSE；macOS 需要 macFUSE 内核后端。

**违反示例**：写入的只是普通 RAM 文件、缺失父层被当作零、manifest 未生成新的 head、Seekable 索引损坏、guest 缺页读回压缩字节而不是原文、回收后可变页面丢失，或 compaction 后旧层持续累积。

运行与断言：[Scrut S-DOC-062](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/vm/S-DOC-062.md)。

## VM 检查准备

下列准备块声明硬件前提并保留 SDK 失败诊断。

准备与断言源码：[fixtures/vm.bash](https://github.com/DeepLink-org/pvisor/blob/main/tests/spec/fixtures/vm.bash)。
