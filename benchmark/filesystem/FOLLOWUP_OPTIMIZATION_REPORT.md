# VM 文件系统后续五项独立实验

## Motivation

在PR #100的初轮候选之上，逐项检验：已解析lazy对象复用、流式READ、
V2逻辑内容缓存、直接填充upper、按修改范围失效名称快照。目标是减少
剩余工作量，不把“减少一次复制/查询”直接当成任务加速。
五个新开关均默认关闭；结果未检出收益的项不作性能推荐。

## Experiment design

复用B-FS-ENG注册条目。每项独立N30配对、seeded随机交替、每fresh VM/
容器三轮，先在环境内累计再计算配对比值；5,000次配对bootstrap给逐项
95%区间，未校正多重比较。每项另跑两份N1插桩诊断，完整检查实例注册和
唯一final，计数不进入时钟。Linux x86_64/KVM，2vCPU/512MiB guest，
宿主CPU0,1 affinity/200%quota、2GiB父预算、MemorySwapMax=0。
宿主其他进程、全局swap及宿主缓存不受该预算控制；无外部干扰筛选，
保留有效慢值和回退，不报告P95/P99或冷磁盘结论。

前四项通过真实Docker-style CLI和本地`FROM scratch/COPY` V2镜像，
不下载registry输入；固定逻辑目录镜像、名称缓存和prepared-copy-up开启，
每项只切换自身新开关，其他后续项关闭。第5项是Immutable物理lower SDK
目录场景，名称缓存固定开启，前述后续机制关闭。相互不累加百分比。

| 项 | 开关 | 负载与主要指标 |
| --- | --- | --- |
| 1 | `PVISOR_VM_FS_REUSE_LAZY_NODES` | 32×64个8字节文件，加7层路径；三轮stat累计 |
| 2 | `PVISOR_VM_FS_STREAM_LAZY_READS` | 8×4MiB高熵文件；三轮重开读累计，逐字节校验在每读计时外 |
| 3 | `PVISOR_VM_FS_LOGICAL_DATA_CACHE` | 256×64KiB文件；每轮4次重开读累计，三轮合计 |
| 4 | `PVISOR_VM_FS_DIRECT_LAZY_COPY` | 三轮各8MiB新源文件；可写OPEN、8字节修改、文件和父目录fsync完成 |
| 5 | `PVISOR_VM_FS_SCOPED_NAMES` | 8192长名称目录；无关目录create→rename→unlink后，在guest内drop_caches=2强制reimport，三轮累计 |

第3项先以候选跑真实guest的copy-up、旧FD、upper/lower内容页隔离及unlink
正确性检查，通过后才测静态只读负载。第4项验证完整upper内容，Core合同
另检验普通/准备两条复制路径不物化lower，以及partial EIO不发布upper。
第5项完整检查名称、重复项和修改范围；不改变宿主cache或策略，
不代表自然编辑器缓存命中率。

每批保存源码、差分、制品、固件SOURCE、显式环境/argv、原始记录和状态。
本地批次（不随仓库分发）分别为`benchmark/.data/lazy-nodes-followup-01`、
`read-sink-followup-01`、`logical-data-followup-01`、`direct-copy-followup-01`、
`scoped-names-followup-01`。固件与初轮一致。最终代码整理包含等价配置解析
归并、格式和边界测试；以下数字只归属各自冻结制品，不代表最终组合已重测。

## Data and analysis

正降幅表示更快。配对降幅不是两个中位数之比。

| 项 / 主要指标 | off中位ms | on中位ms | 配对耗时降幅 | 95%区间 | 结论 |
| --- | ---: | ---: | ---: | --- | --- |
| 1 深路径stat | 397.273 | 370.343 | 9.43% | [5.83%, 11.72%] | 改善 |
| 2 重开大文件读 | 30.206 | 28.973 | 4.17% | [-1.61%, 7.44%] | 未检出 |
| 3 重开小文件读 | 1038.033 | 642.026 | 38.15% | [37.88%, 38.31%] | 改善 |
| 4 copy+文件/目录同步 | 33.549 | 29.864 | 10.42% | [8.89%, 12.45%] | 改善 |
| 5 名称reimport | 208.973 | 207.937 | 0.83% | [-0.15%, 1.37%] | 未检出 |

### 分项证据及范围

- **1：** 同批读回累计也有6.96%的改善信号，[1.87%,13.52%]。实现只复用
  同一投影已完整遍历并发布的正项，使用既有path/inode表；未新增第二套
  属性缓存，不以预取表或缺项证明祖先安全。无并发吞吐或Agent整体结论。
- **2：** 首次读取降幅0.41%，[-1.20%,1.70%]，也未检出。去掉中间Vec
  并未在此端到端读负载中兑现明确收益。仍保留普通native FD零拷贝路径、
  verified block/EOF/错误及RAM lease契约；不称端到端zero-copy。
- **3：** 独立诊断READ由3847→263，OPEN3843保持；LOOKUP382→196。
  内容ID来自稳定逻辑租约内的backend对象与长度，不用native占位inode
  作为内容证明；native与逻辑key域分离，仍校验实际只读FD形态。受测
  16MiB复用工作集适合guest页缓存；未测较大工作集、内存压力和多VM密度。
- **4：** OPEN单项下降29.64%，[28.30%,30.18%]，但包括同步后的主要指标
  只下降10.42%，不能把提前返回当作完整收益。诊断每候选环境24MiB直接
  拷贝、基线0；两条路径都保留元数据和原像顺序，lower物化能力不删除。
  只测本地lazy存储，不外推远程网络下载。
- **5：** 完整编辑循环下降0.85%，[-0.32%,1.50%]，同样未检出；N5 stat、
  scan和热open控制均未检出差异。诊断虽记录3次保留，但实际命中4/未命中5
  与基线相同：范围保留没有转化成此负载更少的名称重建。保留这个负结果，
  不据此继续盲目扩容或默认开启。guest rename镜像协议未修改。

### 代码整理与后续门槛

流式读取复用一个verified-block交付循环；配置只在owner构造时读取并严格
验证。逻辑内容ID有界且不回收、与namespace/upper隔离；direct copy只面向
私有临时upper，失败保留源可见性。名称范围在namespace锁内解析，父目录/
子树先失效，属性修改、未知范围、非默认策略及capture仍保守全失效。
没有默认启用候选、合入main或将私有性能样本改写成用户benchmark排名。
合版前仍需平台CI、最终制品回归和组合/真实Agent任务验收。

整理后本地检查：

- `mise run test -- pvisor-vm pvisor-overlay-core pvisor -- --lib --tests
  --no-fail-fast`：1676通过、24跳过。
- `mise run test-py -- benchmark/filesystem/test_directory_names_ab.py
  benchmark/filesystem/test_logical_directory_ab.py`：6通过。
- `mise run lint-rust`：workspace/all-targets严格Clippy通过。
- `mise run lint-api-docs`：warnings-denied rustdoc通过。
- `mise run ci -- "fmt-rust --check" "fmt-py --check"`、filesystem Ruff及
  `git diff --check`通过。
- 全部五批status均valid，failures/excluded均0；成功SDK夹具已回收，
  未再次产生默认`/tmp`大文件夹具。
