# 0006：文件快照保存 stage，复用不可变基底

**状态：accepted，首版 CLI profile 已实现。** 2026-10-04。manifest v4/v5 与 `pvisor snapshot` 已保存完整 stage 和独占基底引用；旧 v1–v3 完整环境恢复继续支持。普通 Job checkpoint transport、workspace 接入和 stage/RAM 增量保存仍未实现。

## 背景

完整文件树快照在保存和恢复时遍历、校验整个 rootfs。逐文件 clone/reflink 减少数据复制，但目录与完整内容校验仍随基底规模增长。Agent 分叉应主要为执行产生的 stage 付费，基底由镜像或输入 revision 复用。

## 选择

文件 checkpoint 保存可写 stage 及其恢复和审查语义：upper、whiteout、opaque 目录、hardlink/copy-up 状态、必要事务记录、preimages 与 absence observations。只读 lowers 记录有序不可变 generation 引用，不在每次快照中复制整份基底。执行 checkpoint 另外组合同一 epoch 的 CPU、RAM 和设备状态；文件 checkpoint 的成本不能与整机恢复混为一谈。

基底的首次导入、身份验证和 cache 准备单独计量。可变宿主目录须先固定为 revision，或拒绝该快速 profile。路径、镜像 tag 和 chmod 不足以证明不可变。保存对象、恢复实例与分支各自 pin 依赖；基底缺失时按身份补齐或明确失败。

## 后果与迁移

stage 是逻辑状态，不等于仅复制 upper 目录。可写 workspace 也须保存 stage，外部 apply target 重新绑定并继续冲突校验。第一版保存完整 stage，后续才设计 stage mutation 增量；既有 preimage journal 不能替代完整 dirty journal。

原 full-environment 格式维持读取兼容。新 manifest/profile 明确区分基底引用与独立完整副本，旧 reader 对不理解的格式拒绝。只有 stage 与 RAM 均取得不再受源写入影响的版本，才允许在 freeze 之外完成摘要、压缩和持久发布。

## 验收

固定 stage/RAM、增长 base，warm save/fork 不应遍历或复制不变 base；再测 stage 大小、条目数、大文件 copy-up 和 1/8/64 分支。保持分支独立、删除/metadata/hardlink、打开句柄、依赖 GC、损坏拒绝和发布失败的契约。100 ms 等预算需注明规模、预备、持久化与 first useful work，不是本记录承诺。

运行时依旧遵循唯一 `api`、跨平台一致 trait/struct、私有后端实现；镜像解析、base pin 与 store 发布不进入 VMM。见[完整环境快照的当前实现](../environment-snapshot.md)、[OverlayCore](../overlayfs.md)和[ADR 0005](index.md#adr-0005)。
