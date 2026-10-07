# pVisor 实验性特性

围绕“提升执行密度，降低监督成本”，登记可实施、可验证的实验能力。这里保存设计、实施计划与验收记录；功能代码仍归属现有 crate，性能测量仍归属 benchmark 注册表，避免形成另一套产品实现。

## 能力登记

| ID | 能力 | 状态 | 目标 |
|---|---|---|---|
| EXP-001 | [vCPU 空闲观测与自动内存卸载](vcpu-idle-offload/README.md) | M0：观测原型（KVM 实测，HVF 未验收） | 不修改 Agent 和 guest 内核，利用 VM 等待窗口自动 offload，并由宿主事件/deadline 恢复 |

## CLI 特性入口

```bash
pvisor feature
pvisor feature list --json
pvisor run --executor vm --feature vm-vcpu-observe -- /bin/sleep 10
```

运行时 registry 在 `crates/pvisor/src/features.rs`，集中定义稳定名称、阶段、默认值与描述。`--feature NAME` 可重复或逗号分隔，默认不启用，未知名称明确拒绝；`--` 后保留给工作负载。查询中的 `enabled` 只表示注册默认值叠加本次 CLI 启用，不加载 Run 配置或验证实际运行。

第一项注册为 `vm-vcpu-observe`，阶段 `experimental`、默认 `false`，开启 EXP-001 M0 观测，不开启自动卸载。最终 executor 必须为 VM；平台范围为 Linux x86_64/KVM 与 Apple Silicon macOS/HVF，HVF 尚未运行验收。配置支持 `[features]` 的 `vm-vcpu-observe = true`；CLI 启用覆盖配置的 false，省略保留配置。没有 CLI disable 或持久 feature enable 操作。既有 cold RAM 等参数不在本次迁移范围。

功能选择通过 Host 请求、RunConfig、runner spec 显式传递；native runner 实际调用 `VcpuObservationControl`。新增实验能力需先完成真实接入和支持范围检查，再加入 registry，不能登记空开关。用户接口详见[CLI 参考](../docs/src/zh/reference/cli.md#features)。

## 状态与晋级

- **方案登记**：明确问题、现有基础、缺口、范围、失败语义与验收门槛；不表示已实现。
- **观测原型**：只采集机会与拒绝原因，不自动改变执行状态。
- **受限 PoC**：默认关闭、显式启用，在列明平台/profile下实现闭环。
- **实验可用**：正确性与故障门禁通过，有可追溯制品和适用范围。
- **稳定或退役**：经过 API、文档、交付及维护评审后晋级；无净收益或维护成本过高时退役。

每项能力分别维护各平台状态，不用一个平台的 PASS 替代其他平台。升级必须附源码/制品身份、命令、通过/失败/跳过与未覆盖项。

## 目录规则

1. 每项能力用稳定 ID 和描述性子目录，README 记录当前状态与阅读入口。
2. 设计与测量、已实现与拟议接口、启发式与确定保证分别表达。
3. 默认不启用；未经验证的选项不得进入正式 CLI 示例或作为已交付能力宣传。
4. 优先复用现有 Runtime、控制 owner、CPU/device quiescence、存储和证据模型，不另造终态所有者。
5. 公共 VM 接口遵守 `crates/pvisor-vm/README.md`；不同后端支持范围明确，不把 KVM/HVF 或映射形态混为一谈。
6. 实现前逐级阅读目标目录 README。测量前遵守 `benchmark/README.md`，登记或明确复用适当实验协议；原始数据放 `.data/`，不能把手动操作数据当自动策略收益。
7. 语义规格遵守 `tools/semspec/DESIGN.md`，PASS 与人工批准分开；AI 不操作真实审批账本/快照，不弱化 claims/checks/xfail。
8. 本目录不保存真实凭据、用户轨迹、VM镜像或构建产物。

## 第一项能力

EXP-001 优先采用 host-driven 方案：后端提供 vCPU 观测，宿主聚合等待窗口并仲裁资源操作；guest PV 协作保留为可选增强，不是第一版前提。

- [设计与边界](vcpu-idle-offload/design.md)
- [实施计划与验收](vcpu-idle-offload/implementation-plan.md)

EXP-001 已实现默认关闭的 M0 观测 API、后端接点与真实 VM 实验。KVM 实测未发现可靠等待窗口；自动卸载、wake/deadline 闭环仍未实现。详见[第一版实现与验证](vcpu-idle-offload/validation.md)。
