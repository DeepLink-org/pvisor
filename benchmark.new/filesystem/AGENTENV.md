# AgentENV 后端：共同文件系统操作

本接入回答：相同 payload、输入和原有采样下，两套执行路径处理共同文件操作需要多久？
注册为 B-FS-TOOLS；本文只说明后端接入、配置和计时协议。
原版 AgentENV 不改源码、不调整 flush，SDK 固定 E2B 2.50.0。

## 前提与准备

Linux x86_64/KVM、可委派的 user systemd/cgroup v2、Python 3.12+。
安装可选依赖：`python3 -m pip install -r benchmark.new/filesystem/agentenv-requirements.txt`。
默认九后端不需要 E2B，只有显式选择 AgentENV 才加载 SDK。

使用已经创建的原版 AgentENV 服务和 **2 vCPU / 512 MiB** 模板；镜像用户空间应来自
本次 pVisor 工具 rootfs 的相同分发。模板的 Python 需要支持 tarfile data filter；
cc、编译器 cc1、git、python3 的文件 SHA-256 会逐沙箱检查，任意差异拒绝样本。
此适配器不安装服务、不创建模板、不修改全局预算。不要使用旧七工具准备脚本生成此模板。

私有 SETUP.json 示例（替换路径、模板 ID 和实际 cgroup；不要提交密钥）：

```json
{
  "endpoint": "http://127.0.0.1:18080",
  "template": "YOUR_EXISTING_TEMPLATE_ID",
  "api_key_file": "/private/path/agentenv.key",
  "guest_cpus": 2,
  "guest_memory_mib": 512,
  "service_cgroup": "/sys/fs/cgroup/system.slice/YOUR_AGENTENV.service",
  "service_limits": {
    "cpu.max": "200000 100000",
    "memory.max": "25769803776",
    "memory.swap.max": "0"
  }
}
```

服务 CPU placement 要求 0-1；服务预算必须事先由管理员配置并与收据一致。
服务独立的 24 GiB 仅为上述已有部署的示例，**不代表与 pVisor 整系统内存相等**。
模板本身的 CPU/RAM 在 guest 再次验证。endpoint 只允许本地明文 HTTP 或 HTTPS，
禁止 URL 内置凭据；密钥内容不写入协议，只有路径。

沿用 main 的 V2 fixture/payload 准备和 CLI build receipt。准备不会启动测试 VM：

```sh
python3 benchmark.new/filesystem/user_workloads.py \
  --prepare benchmark.new/filesystem/.data/agentenv-NEW \
  --payload-batch PREPARED_V2_PAYLOAD_FIXTURE_BATCH \
  --rootfs TOOL_ROOTFS --artifacts CLI_BUILD_DIRECTORY \
  --backends vm-writeback,agentenv --agentenv-setup /private/path/SETUP.json \
  --samples 3 --timeout 90 --krun-enomem-workaround 0 \
  --toolroot-storage /ssd/NEW_TOOLROOT --stage-storage /ssd/NEW_STAGE
```

所有 NEW 路径必须不存在。`--toolroot-storage` 和 `--stage-storage` 显式选 SSD，
不移动或清理现有实验。KVM workaround 开关必须显式记录，未传时保留产品自动选择。
私有宿主 `/tmp` 上限修正为 1 GiB，按需分配；用于容纳 512 MiB guest RAM backing
及 firmware/boot 映射，不改变 guest RAM 或父组 2 GiB 限制。

已有 authenticated prepared generation 可通过 `--prepared-inputs RECEIPT.json`
选择（同时提供 `rootfs` 和 `workspace`，各含绝对 `store` 与 `base.id`）；必须由
`pvisor cache prepare-rootfs/prepare-workspace` 从相同工具与输入生成，不手改 sealed store。
普通目录与 prepared 配置分批记录。`--runtime-artifacts RECEIPT.json` 支持私有运行时目录，
字段为 `path` 和逐文件 `sha256`，必须含 pvisor、libkrunfw.so.5、payload、workload；
准备及启动核验哈希，拒绝符号链接逃逸，payload/workload 必须与共同工具 rootfs 相同。

`--immutable-input-cache` 是显式工程选项，要求 prepared inputs；不默认开启。
DATA+WB 路径要求 host/firmware 支持相应能力协商；开关表示请求启用，
不能据此认定优化实际生效，需另外核验运行时协商结果。

## 执行与计时

先按需对少量用例预检；预检不计入正式样本，失败保留现场并返回非零：

```sh
python3 benchmark.new/filesystem/user_workloads.py --preflight ABS_BATCH --cases M02,M05
target/semspec/debug/semspec bench --cwd ABS_BATCH ABS_BATCH/in.json
python3 benchmark.new/filesystem/user_workloads_analysis.py ABS_BATCH --no-plots
```

上面三条是人工顺序执行示例，不会在预检失败后自动触发完整测试。
不复用已执行的输出目录。原协议为19用例、每后端每用例 N3 新环境、每环境3轮、
90秒外层 workload watchdog；没有额外预热或追加轮次。创建/上传发生在 workload
计时与 watchdog 之前，使用独立 API 超时；不称为整个生命周期90秒。

适配器用 E2B SDK 创建独立沙箱，上传相同静态 payload 和 fixture，校验哈希，
将输入只读绑定到相同路径，然后一次 commands.run 运行该用例的原有三轮。
每次文件操作由 guest payload 执行，SDK 不逐次代理文件读写。
退出后导出产物，沿用同一 host oracle 校验，finally 只回收记录的 owned sandbox。
创建返回不确定时保留 metadata token 并报错，不删除猜测的沙箱。

| 范围 | 字段/证据 | 是否主指标 |
|---|---|---|
| 共同操作 | guest work_ns | 是 |
| 最终文件/目录同步 | guest final_sync_ns | 单列；原配置的持久化保证可能不同 |
| 创建 / 上传 / guest 准备校验 | agentenv.json 的 create_ns / upload_ns / guest_setup_and_validation_ns | 否 |
| Ready / 执行器准备 | ready_ns / wrapper_prepare_ns | 否；不同后端边界有差异 |
| Bundle 收尾与退出 | exit_after_done_ns | 否；合并区间，未单独插桩 Bundle |
| 导出 / host 校验 / 回收 | output_export_ns / host_validation_ns / cleanup_ns | 否 |

M10/M12 在操作内部显式 fsync，这些 fsync 保留在 work_ns；最终同步列并不代表全部同步。
work 内部既有路径检查、copy-up 等处理仍计入，不声称已经剥离所有产品功能成本。
completed_ns 保留用于时钟一致性核验，AgentENV 对照报告以 work 为主、同步单列，
不生成把 sync-completed 当共同操作的排名图。三轮先逐环境求和，再取 N3 中位数；
失败或缺失的单元不发布中位数，不补零、不拼接批次、不剔除慢有效值。

## 离线验证与来源记录

```sh
PYTHONPATH=benchmark.new/filesystem python3 -m unittest discover \
  -s benchmark.new/filesystem -p 'test_user_workloads*.py'
```

这些单元测试使用 mock，不启动 VM 或 AgentENV。原始数据放 `.data/`，保留协议、
实际命令、guest stdout、每 trial 结果与 oracle、资源收据及来源哈希。
每次测量必须记录实际源码、CLI、固件与配置身份；换版本后不能仅更换结果标签。
性能结果及解释放在对应 PR 描述中，完整原始证据保留在测量批次目录。

新增 upload/export/validation/cleanup 独立字段只用于未来批次；历史记录缺失的字段
保持缺失，不用总时间倒推或填零。
