# pVisor 引入 Rust native guest kernel

引入 Rust native guest kernel 是 pVisor 的长期架构决定。我们可以保持 Git、编译器等现有工具不变，针对 Agent 负载调整 guest 内核与宿主之间的分工。Linux 仍是默认内核，也为这些改动提供了同一 VMM 下的对照。

初步测试已在部分实际任务上看到收益。在同一台 x86_64 KVM 主机上，两种内核都使用 1 vCPU 和 512 MiB 内存。[三轮任务的合计耗时中位数](rust-kernel-write-path.md#rebase-verification-on-main-908aa71c)显示，使用 Rust native 后，仓库巡检从 5.12 秒降到 3.96 秒，解包发布从 17.69 秒降到 14.20 秒。增量编译仍是 Linux 更快：Linux 用时 1.05 秒，Rust native 用时 1.75 秒。

新增的 7 组配对测试中，逐个查询已有文件和不存在的文件时，Rust native 的耗时中位降幅分别为 15.5% 和 24.8%；枚举整个目录时两者基本相同。原始记录保存在 `benchmark/filesystem/.data/native-intro-metadata/`。
