# 设计决策记录（ADR）

ADR 保存设计取舍的背景、备选方案与后果。当前实现合同由对应子系统文章维护；历史决定保留当时的接口、版本和证据范围，不能仅凭 accepted 状态推断所有后续平台与入口已经交付。

| 编号 | 决定 | 保留的边界或代价 | 记录状态 |
| --- | --- | --- | --- |
| 0001 | [计划与实际强制证据分开](0001-plan-and-evidence.md) | 计划最高到 Planned；消费者还需检查执行观察 | 实现回填 |
| 0002 | [捕获与回放独立](0002-capture-and-replay.md) | 执行可独立使用；模型捕获与原生轨迹分别收集 | 实现回填 |
| 0003 | [safe 保留显式执行器选择](0003-explicit-safe-executor.md) | 缺少控制时拒绝，平台前置条件仍需满足 | 实现回填 |
| 0004 | [人工批准语义承诺](0004-human-semspec-approval.md) | 测试结果与产品承诺分别审核 | 现有贡献规则回填 |
| 0005 | [单一 Rust VM crate 与 trait API](0005-rust-vm-api.md) | 后端私有，统一声明不承诺跨架构恢复 | 实现回填 |
| 0006 | [快照保存 stage，复用不可变基底](0006-stage-snapshot.md) | 基底需要验证与 pin；历史独立 CLI profile 不等于当前 Job 接口 | accepted；沿用原记录的 2026-10-04 状态 |
| 0007 | [Host FUSE 与 virtio-fs 共用文件服务](0007-shared-filesystem-service.md) | 共享文件语义，协议状态与并发仍由适配器负责 | 实现回填 |
| 0008 | [Host 与 Guest 控制权限分离](0008-host-guest-control-authority.md) | 两套凭据与端点；Host 内部协议要求精确兼容 | 实现回填 |

“实现回填”说明可以从源码或现有开发规则核对选择，不表示新获得维护者批准。0006 的 accepted 状态沿用其独立记录；其中旧 CLI 与测量仍属于历史制品，当前行为以[环境快照](../environment-snapshot.md)和[Job 检查点](../job-checkpoint-cli.md#10-当前实现与验收边界)为准。

## 如何记录下一项决策 {#new-decision}

每项决策使用唯一四位编号和独立的 `NNNN-短标题.md` 文件，写清背景、备选方案、选择、后果、状态和源码/证据范围。索引只维护选择与影响，不复制正文；同一主题后续改变时，记录替代关系并链接原决定。

状态区分 proposed、accepted、superseded、rejected 和实现回填。代码合入与人工批准分别核对。改变 crate 边界时更新[迁移状态](../architecture.md#api-boundaries)；改变记录格式或提交顺序时同步更新[版本矩阵](../records-and-versions.md)与[失败语义](../failure-semantics.md)，使兼容策略和重试条件能够一起评审。
