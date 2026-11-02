# VM 文件系统顺序优化：本轮进度

## Motivation

按顺序检验四类候选：①目录分页、lazy 对象解析与读复制；
②copy-up 数据路径与 namespace 锁；③逻辑不可变镜像的 guest 缓存资格；
④对象级写屏障和异步完成。每批独立归因，不把收益百分比相加。
本轮实现第一批目录名称快照、第二批prepared-copy-up、第三批逻辑目录镜像，
以及第四批对象级READ屏障原型。前三项完成独立N30对照；第四项仅完成
定向合同测试，尚无性能结论。四个开关均默认关闭，不扩大生产默认资格。

本文记录初轮；第二轮五项独立N30结果与负结果见
`FOLLOWUP_OPTIMIZATION_REPORT.md`，两轮编号与收益不可混用或累加。

## Experiment design

注册条目：`benchmark/REGISTRY.md` 的 B-FS-ENG。冻结基线为
`1a4a8dfb` 加各批独立保存的源码/差分和 release 制品。
正式执行器为 `semspec bench`，不使用测试耗时作为性能。
Linux x86_64/KVM、2 vCPU/512 MiB guest，host CPU0,1 affinity、200% quota、
2 GiB MemoryMax、MemorySwapMax=0；宿主其他进程及全局 swap 不受此预算控制。
宿主 backing 未清缓存；无外部干扰过滤，保留慢值。每 VM 内三轮以一个
独立环境统计，不当作三个独立样本；配对 bootstrap 5,000 次、逐项95%区间。

第一批：同一新 release SDK driver/guest/固件，仅切换
`PVISOR_VM_FS_DIRECTORY_NAMES_CACHE=0/1`。8192 个长名称文件、每轮重新挂载，
校验完整名称集合与重复项，首/中/末三个文件在计时外逐字节核对。
另有每臂N5的stat256、scan300、open10000控制；profile为每臂独立N1，
核对注册实例、唯一final和实际资格，不混入正式时钟。
固件复用宿主已有制品并复制到本工作树，SOURCE中所有patch和x86_64配置
摘要匹配当前源码；固件SHA-256为
`634d73f20c042894afa196fe396e561e9cf6b6a61c2e61fcd961eddb2e5cdf88`。

第二批：固定名称缓存开启，仅切换 `PVISOR_VM_FS_PREPARED_COPY_UP=0/1`。
每轮独立32MiB普通lower文件首次可写打开，与256次强制statx并发，
分记OPEN、metadata和两者完成时间。只研究SDK无journal路径，不代表
live stage或远程lazy下载。确定性受阻backend测试另验metadata可进、
其他namespace修改不可越过；该注入不用于计时。

原始数据仅在本地 `.data/`，不随本报告分发。第一批成功批次为
`benchmark/.data/directory-names-sequential-02`；此前pilot及错误实验变量
导致的失败保留，不补入正式样本。第二批失败批次为
`benchmark/.data/prepared-copy-up-sequential-01`；修复harness后成功批次为
`benchmark/.data/prepared-copy-up-sequential-02`。第三批成功批次为
`benchmark/.data/logical-directory-sequential-01`。

第三批使用真实Docker-style CLI，以本地`FROM scratch/COPY`构建固定V2
镜像，32×64个8字节文件，三轮stat及32×8文件读回分别计时。
只切换`PVISOR_VM_FS_LOGICAL_DIRECTORY_CACHE`，名称缓存和prepared-copy-up
固定1、DATA/FLUSH/WB关闭；`run --rm --network none`，无远程镜像下载。
独立profile环境暂留命名实例，保存日志并核对注册/final后才删除本次实例。
物理镜像lower仍Mutable，稳定逻辑租约不授予物理缓存或snapshot资格。

## Data and analysis

### 第一批目录名称快照：宽目录有收益，其他控制未检出

| 三轮累计指标 | 每臂环境数 | off中位ms | on中位ms | 配对耗时降幅 | 95%区间 |
| --- | ---: | ---: | ---: | ---: | --- |
| 8192项目录枚举 | 30 | 458.175 | 410.509 | 10.87% | [9.95%, 12.19%] |
| stat256 | 5 | 7.515 | 6.929 | 7.60% | [-8.97%, 14.95%] |
| scan300 | 5 | 40.335 | 38.603 | -4.17% | [-16.31%, 18.52%] |
| open10000 | 5 | 25.382 | 25.274 | 0.80% | [-0.98%, 35.29%] |

降幅由每环境候选/基线比值计算，不等于两个中位数之比。区间未作多重
比较校正，N5控制不足以估计尾延迟或稳定分布。主要指标28/30对改善，
两对回退，全部保留。该机制不减少逐子项属性校验，仅消除重复名称构建。
独立计数中stable share的缓存命中3、未命中3；Mutable share实际未启用。
这是目录导入的机制收益，不是CLI、Docker或完整Agent任务收益；
lazy对象解析和中间读复制尚未实施。

### 第二批：修复harness后，复制与metadata重叠有收益

| 三轮累计指标 | 每臂环境数 | off中位ms | on中位ms | 配对耗时降幅 | 95%区间 |
| --- | ---: | ---: | ---: | ---: | --- |
| OPEN与metadata完成（主要） | 30 | 63.367 | 55.243 | 11.19% | [6.99%, 21.83%] |
| metadata | 30 | 63.073 | 52.565 | 15.71% | [6.95%, 32.25%] |
| OPEN | 30 | 27.855 | 28.108 | 3.58% | [-9.08%, 8.94%] |

新批次60个正式环境和两个独立诊断环境全部通过，每环境退出后从外层校验
96MiB lower与96MiB upper的完整字节，再清理自有夹具。OPEN单项未检出改善；
N5的stat、scan及open控制也未检出差异。配对分布较宽，保留全部点，
不将响应性改善等同于复制吞吐加速，更不外推远程下载或完整构建。

#### 保留的无效历史与清理

原计划30对；第17对候选超过90秒，整批标记invalid，不以已完成的16对
计算收益。确定性并发测试通过只能证明所测锁边界，不能替代真实VM验收。

排查发现既有SDK probe调用的VmRuntime在guest关机时结束整个runner，
不会返回到TempDir析构或其后的host oracle。旧harness使用默认`/tmp`
（本机为tmpfs），因此持续保留夹具；发现本轮35个copy-up夹具和67个
目录分页夹具，共约8.91GiB已分配存储，宿主内存压力上升。
这是已确认的测量环境污染；尚未排除其他超时原因，不把它认定为
prepared-copy-up本身的性能或正确性结论。

外层harness现已改为批次`work/`内的TMPDIR，成功退出后从外层校验
copy-up的全部lower/upper字节，再只清理该成功试验的自有目录；
失败现场保留。新增离线测试覆盖末尾字节损坏、长度变化、夹具歧义和
失败保留。后续新批次已成功，旧批次不补样或重写。

经用户确认，仅删除清单中的102个外部临时目录，释放约8.91GiB；
最新copy-up夹具先保留到工作树。清单与删除收据分别在本地
`benchmark/.data/fs-sequential-cleanup-inventory.json`和
`benchmark/.data/fs-sequential-cleanup-receipt.json`，其他`/tmp`内容未删除。

### 第三批：固定V2镜像的逻辑目录镜像

| 三轮累计指标 | 每臂环境数 | off中位ms | on中位ms | 配对耗时降幅 | 95%区间 |
| --- | ---: | ---: | ---: | ---: | --- |
| 2048文件stat（主要） | 30 | 592.082 | 267.712 | 53.21% | [51.33%, 55.65%] |
| 256文件读回 | 30 | 262.204 | 175.958 | 10.15% | [1.09%, 29.67%] |

独立诊断中LOOKUP为2094→106，READINDEX为1→34；GETATTR529、
OPEN771、READ775和FLUSH770均保持。计数支持目录查询下沉，
不能用来精确拆分read阶段耗时。CLI进程时间中位数为1066.141→640.504ms，
包含相同systemd启动/CLI/退出开销，不是纯VM启动时间，未单独作区间归因。
正式60个容器与两个诊断容器全部正常退出。只验证固定、断网、静态payload
工作流；联网resolver、完整Agent、多VM密度和macOS/HVF尚未验收。

### 第四批：对象级READ屏障原型，尚未测量

`PVISOR_VM_FS_OBJECT_READ_BARRIERS`仅改变host-writeback READ的等待范围。
不同FD/硬链接按固定native upper inode统一等待；旧lower FD不等待upper
写入，但仍检查session sticky error。单FIFO worker、metadata/OPEN、
FLUSH/FSYNC/RELEASE和capture的全局屏障保持；不是并行写完成或完整IOCP。
pending对象索引受已接收任务预算约束，完成即移除，关闭开关不维护索引。
定向测试覆盖无关READ可推进、同对象FD别名等待、索引回收与sticky错误。
尚未跑真实VM的混合I/O性能批次或offload验收，不报告收益。

### 代码与检查状态

- 目录名称缓存：每owner最近一个目录、约4MiB分配容量上限、只保存名称/
  类型、不保存属性/授权/lookup引用；物理lower全部Immutable才启用；
  namespace/metadata写锁在成功或失败修改前失效，打开目录保留Arc快照。
- prepared-copy-up：独立mutation admission串行化修改/捕获/销毁，复制准备
  不持namespace独占锁；发布继续验证既有Core receipt及原有I/O fence。
  admission保持到receipt取消后。只覆盖writable OPEN，其他copy-up原路径保留。
- 四个候选默认关闭；保持API声明和snapshot格式，不把进程内逻辑租约保存
  到snapshot。相关PR尚未取得合入授权。
- 基线 `mise run test -- pvisor-vm -- --lib`：606通过、8跳过。
- 第二批完成时 `mise run test -- pvisor-vm -- --lib --tests`：634通过、9跳过。
- 第三批新增backend稳定租约合同3项及VM资格/语义定向2项通过；
  第四批对象屏障、索引回收和严格配置定向3项通过。
- `mise run test-py -- benchmark/filesystem/test_directory_names_ab.py`：
  3通过；新增三个Python文件的Ruff检查通过。
- PR整理时，对VM、OverlayCore与runtime的`--lib --tests`聚合检查：
  1669通过、24跳过；两份新增Python合同测试共5通过。
- `mise run ci -- "fmt-rust --check" "fmt-py --check"`通过；新增Python
  sampler/analysis/test的Ruff检查通过。
- `cargo clippy --locked -p pvisor-vm -p pvisor-overlay-core -p pvisor
  --lib --no-deps -- -D warnings`通过。早期单独VM默认features检查出现过
  未修改的`NetworkStream.inner` dead_code，未通过抑制或修改该告警取通过；
  聚合命令覆盖的是实际runtime依赖组合，二者不是相同的feature集合。
- 未运行workspace全量测试、macOS/HVF或最终端到端CLI/Agent任务验收。
