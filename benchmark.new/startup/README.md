# 启动时间：一张表，一个 JSON

这里回答的问题：环境已准备好时，各执行模式到首条 Ready 输出与正常退出要等多久？
注册条目为 [B-STARTUP](../../benchmark/README.md#b-startup)。

[timing.json](timing.json) 是八行对比表，每行调用 `<backend>_timing.py`；
各后端也有独立的 `<backend>_timing.json`，输出与输入同名的 `.render.json`。

```sh
just benchmark startup/timing
just benchmark startup/timing --row Docker
just benchmark docker_timing
just benchmark pvisor_vm_timing
just benchmark native_timing
```

`--row` 只执行并渲染选中的行。默认使用自动清理的临时目录，只保存最终表格。
需要保留原始证据或发布文档时：

```sh
just benchmark startup/timing --output benchmark.new/startup/.data/measurement
python3 benchmark.new/update.py benchmark.new/startup/.data/measurement/row-0-* --startup-docs docs/src
```

每行证据保存在独立的 `row-<index>-<id>/`，显式输出目录拒绝复用。
`update.py` 的本地 CSV / Markdown 默认放在 `.data/results/`。

## 制品与输入

非原生后端通过 `--assets PATH` 或 `PVISOR_BENCHMARK_ASSETS` 指定共同输入；
FC/QEMU 通过 `--kernel-receipt PATH` 或 `PVISOR_BENCHMARK_KERNEL_RECEIPT` 指定 stock 内核收据。
代码不绑定历史批次路径或日期。Docker 默认使用输入 `assets.json` 的不可变 `docker_image`，
也可通过 `PVISOR_BENCHMARK_DOCKER_IMAGE` 指定；镜像必须预先存在。

pVisor 默认使用 `target/release/pvisor`，可用 `--binary PATH` 指定。
已有 Host listener 必须与二进制匹配。默认选当前可用的前两个 CPU；
`--cpu-affinity` 可指定准备和采样共同使用的亲和性。

## 当前运行链路

- `subjects.py`：八个后端的名称、标签和固定被测 argv。
- `common.py`：展开输入、核对制品、准备工作区并执行预检。
- `hooks.py`：重置每次输入，校验输出、canary、Run Bundle 和只读 stage。
- `timing.py`：共享事件时钟、采样和统计；各 `*_timing.py` 固定后端。
- `audit.py`：复算当前批次的配对样本并校验逐次证据。
- `publish.py`：将成功批次发布为双语 Ready / Exit 结果 JSON。

公开 CSV 同时保留 Ready 与 Exit 两列；用户页只引用 Ready 表，因为退出时间不改变读者的选型。来源摘要共用
`docs/overrides/assets/benchmarks/startup-timing.provenance.json`，单行复测保留其他
条件的批次和配置。完整 manifest、输入清单与内核收据留在试验 `.data/`。

每行固定 3 次预热、60 次正式样本，VM 为 2 vCPU / 128 MiB，热缓存。
时钟在 Popen 前开始，收到唯一且精确的 `REFERENCE_READY` 行时记录 Ready，
正常零退出时记录 Exit。两个指标来自同次运行，使用标准库样本标准差。
解释器启动和输入准备不计时；Popen、管道观察与调度包含在内。

预检、预热与正式样本都要求正确有序输出、输入未变和零退出。
pVisor 还校验实际隔离模式与 Run Bundle；FC/QEMU 要求有序 Exit0 且无 panic。
FC 使用 stock 内核/MMIO，受控停止不能算正常退出。任何失败都阻止更新结果。
日志直接保存在试验目录；成功验证后丢弃可重建磁盘，保留纳秒记录、输出与 Bundle。

历史报告、CSV 和旧 results 已归档到 `.data/history/`，不参与当前采样、审计或发布。
工程与诊断占位表仍由 registry 登记，尚未准备的负载拒绝执行。

## TurboOCI 按需镜像对照

[TurboOCI 对照协议](TURBO_OCI.md)属于 B-LAZY-STARTUP，分别比较客户端准备、
Ready、正常退出和传输成本，与上面的已准备环境热缓存启动表保持独立。
独立 OverlayBD/TCMU 环境已部署，操作和功能验证见
[部署记录](OVERLAYBD_LAB.md)。`lazy_startup.json` 仍为未准备的占位表，
正式四组采样与审计尚未接通，不发布新成绩。

```sh
python3 benchmark.new/startup/turbo_oci_preflight.py --help
python3 benchmark.new/startup/turbo_oci_preflight.py --binary-dir target/release
```

预检只读取设备、工具及制品状态，不安装依赖、加载内核模块、启动服务或清理缓存。
只有所有前提通过并完成专用采样与审计接线后，才能执行正式对照；
预检通过本身不证明挂载、镜像导入或隔离已验证。

### 已准备镜像的性能对照

`image_compare.py` 比较同源 Ubuntu shell 在普通 OCI 容器、TurboOCI 容器和
pVisor 本地预准备 rootfs VM 中的 Ready / Completion；不包含镜像拉取、转换或
解包。每组 3 次预热、30 次随机交替正式启动，专用审计复算时钟、Bundle、
制品、清理和后台负载，生成本批次的 Markdown/CSV。

```sh
python3 benchmark.new/startup/image_compare.py \
  --prepare benchmark.new/startup/.data/image-compare-prepared-new \
  --lab benchmark.new/startup/.data/overlaybd-lab-20261017 \
  --prepared-store benchmark/pvisor/.data/lazy-startup-local-20261007/service-store \
  --binary-dir target/release --cpus 12,13 --service-cpus 14,15
target/semspec/debug/semspec bench \
  --cwd benchmark.new/startup/.data/image-compare-prepared-new \
  benchmark.new/startup/.data/image-compare-prepared-new/in.json
python3 benchmark.new/startup/.data/image-compare-prepared-new/harness/image_compare.py \
  --audit benchmark.new/startup/.data/image-compare-prepared-new
```

实验目录必须全新，不能并行复用同一 lab。采样包含容器的 Docker exec 客户端，
VM 的 2 GiB guest RAM 与容器硬限制不代表等整机内存预算。宿主页缓存保留，
其他 CPU 上的后台负载单列；不清理或停止其他测试。
结果保留在 `.data/image-compare-prepared-20261017-a/`，含 99 次完整启动证据、
专用审计和 `report.md`；这是独立 prepared scope，不填充仍未准备的四组
`lazy_startup.json`，也不通过通用 `update.py` 发布为用户 benchmark 表。

### 相同 stage 执行器的惰性文件加载 {#stage-lazy}

`stage_lazy_compare.py` 将两种按需 lower 都交给同一个 pVisor host staged
执行器，宿主工具按相同顺序读取并校验四个镜像文件，共 2,575,738 B。
一侧是公开 cache API 的 host FUSE，另一侧是 TurboOCI/OverlayBD 的只读
ext4 挂载；两侧均使用相同 Docker exec 包装器、CLI、探针与应用 CPU。
它不执行镜像内的程序，不代表 CLI 已支持 host executor 的 OCI rootfs。
`--stage` 只暂存工作区；不能用 `--executor host --rootfs image=…` 冒充这个路径。

```sh
python3 benchmark.new/startup/stage_lazy_compare.py \
  --prepare benchmark.new/startup/.data/overlaybd-lab-20261017/stage-lazy-new \
  --lab benchmark.new/startup/.data/overlaybd-lab-20261017 \
  --prepared-store benchmark/pvisor/.data/lazy-startup-local-20261007/service-store \
  --binary-dir target/release --runtime-rlib target/release/deps/libpvisor-<hash>.rlib
target/semspec/debug/semspec bench \
  --cwd "$PWD/benchmark.new/startup/.data/overlaybd-lab-20261017/stage-lazy-new" \
  "$PWD/benchmark.new/startup/.data/overlaybd-lab-20261017/stage-lazy-new/in.json"
python3 benchmark.new/startup/.data/overlaybd-lab-20261017/stage-lazy-new/harness/stage_lazy_compare.py \
  --audit benchmark.new/startup/.data/overlaybd-lab-20261017/stage-lazy-new
```

`--runtime-rlib` 指定现有 release 构建的 runtime 库，仅链接 benchmark owner
helper，不改产品 API。准备时核对原始 OCI TAR 中的文件摘要，冻结工具和
harness。正式入口为单个 semspec Python 配对行，3 轮预热、30 轮随机配对；
每组冷客户端后紧接热客户端，每次均使用新挂载和新 stage。只重置本 lab 的
私有客户端状态，保留 registry 源内容及宿主页缓存；两种服务均在本机，无 WAN。
OverlayBD 使用官方带 lease 的 `images mount/unmount`，并在必要时正常卸载
父挂载、通过官方 attacher 释放对应设备。每次退出后重新审计设备清单。

完整 Ready/Completion 包含客户端打开或 rpull、挂载、stage CLI 和正常退出，
另记录已挂载后的 stage Completion。应用固定 CPU 12,13，镜像服务固定
14,15；没有声明等整机内存预算。任何校验、退出、冷暖取数、实际后端、清理
或预定干扰门禁失败均使整批无效，不剔除慢样本。原始证据和报告保存在该
batch 下，不与原 VM/容器批次拼接，也不通过通用发布器覆盖用户表。

已完成批次位于
`.data/overlaybd-lab-20261017/stage-lazy-20261017-a/`，包括 132 次执行、
逐次原始日志、`audit.json`、`summary.json`、`samples.csv` 和 `report.md`。
独立离线审计通过，并已核对 semspec 导出与复算结果一致。
正式输入、两种原始/派生 OCI 身份和 helper 链接收据均保留在该批次；
功能预检失败记录留在独立目录，未混入正式数据。
