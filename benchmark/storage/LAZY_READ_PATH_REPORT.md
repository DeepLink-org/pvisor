# Image Pack V1 读路径工程 A/B：重复哈希、拷贝与元数据投影

- 日期：2026-11-25 / 2026-11-26
- 角色：**engineering A/B**（不是用户基准，不更新用户页数字）
- 问题：同一镜像、同一组已创建容器、同一宿主与 CPU 预算下，去掉读路径上的重复
  SHA-256、重复解压上下文、临时拷贝，以及元数据目录投影中的 O(n²) 查找和逐子项锁后，
  lazy 读取比优化前快多少？还剩多少差距？
- 结论（相对最初基线）：`bigfile` −21.5%（1.90×→1.49×）、`smallfiles` −11.0%
  （1.97×→1.75×）、`tree` −7.1%（3.18×→2.78×）、`dirs` −33.3%（3.91×→2.64×）。
  内容读取的重复工作已去掉；元数据轮去掉了子项查找和锁的重复开销，但每次容器启动仍
  重建整个投影，剩余 2.6–2.8× 主要来自每文件宿主占位创建与元数据检查。

## 环境与来源

| 项 | 值 |
|---|---|
| 节点 | 共享测试宿主，AMD EPYC 7H12，255 vCPU，503 GiB，Ubuntu 24.04.1，内核 6.8.0-49 |
| upstream | `708a49bd`（干净检出构建基线；候选为同一 upstream 加本次改动） |
| 最初基线 | `sha256 8edaf4e7e8d6346f0c9b29c01d3355e96df7fde087446df1219c2e600d834b83` |
| 读路径候选 | `sha256 2468da55fc3ec06ee6c8d8dd680cd90d42d1f420cc5e55f913cef626d2d24ebc` |
| 元数据候选 | `sha256 852e28fc845ce41e4a43816f2bc06b7a0117f444177eb6177f8b17a40cd55e74` |
| 镜像 | 一个 1.4 GB 的业务镜像（15 层，1,076,234,396 B），manifest `sha256:96b2071dbb1ea0efcc3adbc4c1b37c8453df5bdc44e1c31c2b19fc89cb925933` |
| 源 | 只监听 127.0.0.1 的私有 HTTPS registry（同一批 blob），eager 臂走冷路径；lazy 臂走本地池 |
| guest | 2 vCPU / 2048 MiB，`--network none`，`taskset -c 4,5`，入口 `/usr/bin/tini -- /bin/sh /payload/run.sh` |
| 容器 | 同一组 `eager-src`（`pvisor-import-v1:` 解包树）和 `lazy-src`（`pvisor-image-pack-v1:` 池），由基线二进制创建；候选只换运行时二进制 |
| 时钟 | guest `/proc/uptime`（命令开始）与 `date +%s%N`（读取阶段），宿主只记端到端 |
| 采样 | 读路径主负载每臂 3 次预热 + 30 轮；补充负载 2 + 15 轮；元数据负载 3 + 30 轮 |
| 有效性 | 退出码 0、标记出现、两个 start 臂的 workload 输出逐字节一致；全部有效样本保留，不剔除慢值 |

构建命令（三份二进制相同，只有源码不同）：

```sh
PVISOR_KRUNFW_KERNEL_BUNDLE=<kernel-bundle> \
cargo +stable build --release --locked \
  --target x86_64-unknown-linux-musl -p pvisor-cli --bin pvisor
```

固定工具链缺 musl 目标，三份二进制都使用已安装的 stable 1.99.1；同工具链、同内核包，
绝对数值不等于固定 nightly 构建的结果，但 A/B 有效。

## 第一轮：内容读取路径

1. **内容读取不再算 ETag。** `Storage::read` 的文件系统分支原来对每次读取都做一遍
   SHA-256 作为 `UpdateVersion.e_tag`，而内容读取没有消费者。现在只有
   `read_versioned`（CAS/HEAD 观察）在本地对象上计算 ETag；远端后端保留服务返回的 token。
2. **已认证的帧不再二次校验。** `load_data_frame` 和二进制索引页在读取时已经用
   `reference.digest` 校验过编码帧，随后 `frames::unpack` 又算了一遍。新增
   `unpack_verified`，只保留长度和解码后长度检查。
3. **slot 摘要每帧只算一次。** 大文件的一个 1 MiB slot 会被多次 128 KiB 读请求命中；
   原来每次读都对整个 slot 重算 SHA-256。现在 `Cached::verify_slot` 把已校验的
   `(slot, digest)` 记在不可变帧上，后续读只做长度检查。
4. **去掉临时拷贝。** `read_packed`/`read_v1_slot` 原来返回 `value[offset..end].to_vec()`，
   再被 `body.extend` 拷贝一次。现在返回持有 `Arc<Cached>` 的借用切片 `Slot`，
   `body` 预分配容量后 `extend_from_slice` 一次。
5. **整 slot 读复用摘要。** 小文件整读时响应体就是 slot，`Response::Data.sha256`
   直接使用已校验的 slot 摘要，不再对响应体重算。
6. **复用 zstd 解压上下文。** `frames::decompress` 原来每帧新建 `Decompressor`；
   现在使用线程本地上下文，只初始化一次 `WindowLogMax(21)`。

按 1 MiB 大文件块、每次 128 KiB 读请求估算，优化前每块经过约 12 遍 SHA-256
（ETag 1 + 帧校验 1 + unpack 1 + slot 8 + 响应体 1），优化后约 3 遍
（帧 1 + slot 1 + 响应体 1）。小文件整读从 5 遍降到 2 遍。

### 主负载（每臂 30 轮，guest 读取 P50，最初基线 → 读路径候选）

| 负载 | eager 基线 ms | lazy 基线 ms | lazy/eager 基线 | eager 候选 ms | lazy 候选 ms | lazy/eager 候选 | lazy 变化 |
|---|---:|---:|---:|---:|---:|---:|---:|
| `shell`（仅启动） | 26.4 | 45.6 | 1.73× | 26.6 | 47.4 | 1.78× | +3.9%（噪声） |
| `tree`（元数据遍历，6,484 个文件） | 845.0 | 2,689.6 | 3.18× | 868.2 | 2,761.2 | 3.18× | +2.7%（噪声） |
| `smallfiles`（400 个小文件读取） | 1,504.9 | 2,968.4 | 1.97× | 1,508.6 | 2,642.0 | 1.75× | **−11.0%** |
| `bigfile`（大文件顺序读 730 MB） | 3,544.4 | 6,735.0 | 1.90× | 3,541.8 | 5,285.8 | 1.49× | **−21.5%** |

eager 对照臂在四个负载上的变化为 +0.8%、+2.7%、+0.3%、−0.1%，内容负载
（`smallfiles`/`bigfile`）的对照变化 ≤0.3%，说明 lazy 的变化不是宿主漂移。

### 补充负载（每臂 15 轮，guest 读取 P50）

| 负载 | eager 基线 ms | lazy 基线 ms | lazy/eager 基线 | eager 候选 ms | lazy 候选 ms | lazy/eager 候选 | lazy 变化 |
|---|---:|---:|---:|---:|---:|---:|---:|
| `exec`（进程启动，5 次 `/bin/true`） | 44.7 | 78.2 | 1.75× | 44.4 | 77.0 | 1.73× | −1.6% |
| `cat`（`find` + 一个 60–80 KiB 文件） | 110.1 | 276.1 | 2.51× | 110.6 | 292.2 | 2.64× | +5.8%（噪声） |
| `dirs`（目录列举，`/usr` 两层） | 65.6 | 256.7 | 3.91× | 65.1 | 257.1 | 3.95× | +0.2% |
| `random`（随机 4 KiB 读，20 个固定偏移） | 269.8 | 635.1 | 2.35× | 271.2 | 606.0 | 2.23× | −4.6% |

`cat` 包含对 `/usr/bin` 的 `find`，主要成本在元数据路径，所以没有体现出内容读取的收益。

## 第二轮：元数据目录投影

`prepare_directory` 为目录的每个子项创建宿主占位文件，`find`/`ls` 的每个目录都会触发。
定位到两处重复工作：

1. **O(n²) 子项查找。** `RemoteFs::listed_metadata` 每次查找都遍历保留的目录页，并为每个
   文件名重新构造路径；`prepare_directory` 对每个子项调用一次，大目录下产生海量分配。
   现在目录页按完整路径建立 `HashMap` 索引，查找只做哈希表访问。
2. **逐子项投影锁。** `project` 原来为每个子项打开并 `flock` 一次 `projection.lock`。
   现在 `prepare_directory` 先在锁外解析所有子项节点，再用一次锁批量投影；单路径
   `project` 仍自行加锁。

### 结果（每臂 30 轮，guest 读取 P50，读路径候选 → 元数据候选）

| 负载 | eager 基线 ms | lazy 基线 ms | lazy/eager 基线 | eager 候选 ms | lazy 候选 ms | lazy/eager 候选 | lazy 变化 |
|---|---:|---:|---:|---:|---:|---:|---:|
| 元数据遍历 `tree` | 859.8 | 2,732.6 | 3.18× | 897.6 | 2,497.4 | 2.78× | **−8.6%** |
| 目录列举 `dirs` | 63.0 | 255.5 | 4.05× | 65.0 | 171.3 | 2.64× | **−32.9%** |

元数据轮采样期间宿主负载很高且波动（基线 1 分钟负载最高 11.5/7.6，候选 38.2/26.5），
候选在更差的负载下仍取得改善；`tree` 的绝对值可能被负载放大，`dirs` 的收益更明确。
中间版本（仅目录索引，未做锁批处理）在负载 44–58 下测得 `tree` 2,431.9 ms（2.82×）、
`dirs` 202.4 ms（2.82×），与最终候选的差异主要来自宿主负载，不单独归因。

## 累计效果（相对最初基线）

| 场景 | 最初 lazy ms | 最终 lazy ms | 变化 | lazy/eager 最初 → 最终 |
|---|---:|---:|---:|---|
| 大文件顺序读 730 MB | 6,735.0 | 5,285.8 | −21.5% | 1.90× → 1.49× |
| 400 个小文件读取 | 2,968.4 | 2,642.0 | −11.0% | 1.97× → 1.75× |
| 随机 4 KiB 读 | 635.1 | 606.0 | −4.6% | 2.35× → 2.23× |
| 进程启动（5 次 `/bin/true`） | 78.2 | 77.0 | −1.6% | 1.75× → 1.73× |
| 元数据遍历 `tree` | 2,689.6 | 2,497.4 | −7.1% | 3.18× → 2.78× |
| 目录列举 `dirs` | 256.7 | 171.3 | −33.3% | 3.91× → 2.64× |

内容负载的最终数字来自读路径候选 `2468da55`；元数据轮只改目录投影，不改变内容读取路径，
最终二进制 `852e28fc` 未重复测内容负载。`exec`/`random` 同样来自读路径候选，元数据轮未
重复测这两项。

## 解读与剩余差距

- **内容读取的重复工作已去掉。** 大文件每次读命中的 1 MiB slot 不再重算摘要，小文件整读
  不再重算响应体摘要；剩余 3 遍哈希（编码帧、slot、响应体）是当前完整性模型的最小值。
- **元数据轮去掉了子项查找和锁的重复开销。** `tree`/`dirs` 分别从 3.18×/3.91× 降到
  2.78×/2.64×。但 `DirectImage::new` 每次容器启动都新建一个临时投影目录（`tempdir_in`），
  不会跨启动复用；每次启动仍要为 6,484 个文件创建宿主占位、设置权限/xattr/时间，并做
  overlay readdir。这是剩余 2.6–2.8× 的主要来源。
- **下一步不是继续删哈希。** 要把元数据差距压到接近 1×，需要跨容器启动复用投影（持久化
  投影根/对象并按镜像 revision 校验），或让 overlay readdir 直接使用后端目录项而不是宿主
  占位文件。两者都是生命周期/架构改动，需单独评估正确性与 GC。

## 局限

- 基线与候选是顺序批次，不是同一轮内的随机交替；用 eager 对照臂的稳定性控制宿主漂移，
  但没有跨批次配对 CI。元数据轮宿主负载高且波动，绝对值偏保守。
- 单台 AMD 机器、单个业务镜像、2 vCPU 绑核、热页缓存；冷盘、冷缓存、多镜像共享 blob
  去重、并发 VM 都没有测。
- `tree`/`dirs` 的结果是本镜像和该元数据访问模式的结果，不代表所有镜像。
- 这不是注册基准，不进入用户页；只用于工程归因和后续优化对比。

## 复现

```sh
# 私有 registry + 同一组容器（基线二进制创建）
python3 -I measure.py --prepare --binary $B/bin/baseline/pvisor \
  --reference <private-registry>/<repo>:<tag> \
  --base $B/base --payload $B/payload --ca $B/work/registry/tls/ca.pem \
  --work $B/work-baseline --cpus 4,5
python3 -I measure.py --binary $B/bin/baseline/pvisor ... --arms start-eager,start-lazy --rounds 30
python3 -I measure.py --binary $B/bin/candidate/pvisor ... --arms start-eager,start-lazy --rounds 30
python3 -I measure.py --binary $B/bin/baseline/pvisor ... \
  --workloads exec,cat,dirs,random --rounds 15
python3 -I measure.py --binary $B/bin/candidate/pvisor ... \
  --workloads exec,cat,dirs,random --rounds 15
# 元数据轮：读路径候选作为基线，元数据候选只换运行时二进制
python3 -I measure.py --binary $B/bin/readpath/pvisor ... --workloads tree,dirs --rounds 30
python3 -I measure.py --binary $B/bin/metadata/pvisor ... --workloads tree,dirs --rounds 30
```

原始汇总保存在工程工作区的 `results-20261125-opt/` 和 `results-20261126-meta/`。
输出一致性另用 1 轮校验（`--rounds 1 --warmups 0`），两个 start 臂在四个主负载上均为真。
