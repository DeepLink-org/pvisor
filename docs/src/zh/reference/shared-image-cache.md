# 共享镜像缓存与存储后端

用 `pvisor push` 分发本地镜像，用 Rust 缓存客户端查询不可变镜像中的文件。`pvisor-daemon serve --cache` 为 VM 客户端准备 OCI 镜像，并将索引和内容写入本地镜像包池。文件查询使用固定的 `image_handle`，不会访问 OCI registry。

## 本地镜像目录 {#local-image-store}

Docker 兼容命令、未选择远程后端的 spawn 和 cache server 共用一个本地镜像目录。macOS 默认是 `~/Library/Application Support/pvisor/docker/images/`；Linux 默认是 `$XDG_DATA_HOME/pvisor/docker/images/`，通常为 `~/.local/share/pvisor/docker/images/`。这里保留被任务引用的镜像 revision，使用持久数据目录。

显式 Session/cache 镜像目录优先，其次是 `PVISOR_IMAGE_STORE`。只设置 `PVISOR_DOCKER_HOME` 时，所有本地入口使用其 `images/`，保留隔离存储语义；嵌入式 `Store::open(root)` 固定使用 `root/images`，不读取环境默认值。容器状态、Session 记录和可写层仍各自保存。本地镜像根目录及控制子目录必须由当前用户拥有、权限私有（0700）且不是符号链接；不兼容的已有目录会报错，不自动修改权限。

```text
<image-store>/
├── configs/<config-digest>.json
├── image-pack-v1/
│   ├── format.json
│   ├── meta/<image-key>/...
│   └── data/blobs/...
├── staging/.import-*/
├── locks/
└── projections/
```

`staging` 中的下载／解包目录在普通成功或失败后删除，进程崩溃可能留下残余。`configs` 保存按摘要校验的原始 OCI 配置。新本地镜像使用 `image-pack-v1`，已固定的预发布 V2 镜像保留原位置。Docker pull 仍查询 registry，但可复用相同 digest 的共享池内容。build 的各个标签独立提交。`rmi` 只移除 catalog 引用，不回收共享内容；活动和已停止任务保留固定 revision。

Docker Hub 短名和完整名共用一个 canonical catalog 身份，pull/build 替换、create/FROM 查找和 rmi 都按这个身份处理。等价 build 标签只发布一次；不同标签和已有容器保留各自固定的 revision。包含重复 canonical 身份的 catalog 明确报错，需在新存储中重新构建。

本地池标签持续可离线使用，直到显式 pull/refresh 或新的发布替换它。五分钟的标签自动刷新窗口适用于可写的文件系统/S3 后端，不适用于本地池。

镜像记录绑定所选平台与内容池 revision。

## 选择后端

文件系统、S3、Google Cloud Storage 和缓存服务写入 [Image Pack V1](../design/experimental/image-pack/index.md)。新池默认 blob 目标为 32 MiB；需要 64 MiB 时，在写入端设置 `PVISOR_IMAGE_BLOB_TARGET_MIB=64` 初始化独立池。已初始化的池保留原策略。

设置 `PVISOR_CACHE_LOCATION` 时，URL 的 scheme 决定后端，不需要设置 `PVISOR_CACHE_BACKEND`。直接存储后端不需要启动 `pvisor-daemon serve --cache`。未设置仓库 URL 时，使用原有的缓存服务发现规则。

| URL | 权限 | 适用场景 |
|---|---|---|
| `file:///绝对目录` | 读写 | 本机或共享文件系统上的仓库 |
| `s3://BUCKET/PREFIX` | 读写 | AWS S3 或兼容服务 |
| `gs://BUCKET/PREFIX` | 读写 | Google Cloud Storage，通过 Apache OpenDAL |
| `https://HOST/PREFIX` 或 `http://HOST/PREFIX` | 只读 | 支持精确 Range GET 的静态镜像站 |

`PVISOR_CACHE_SERVER=unix://...` 或 loopback `tcp://...` 选择 pVisor 缓存服务地址，不是仓库地址。旧的 `PVISOR_CACHE_BACKEND=filesystem` 加绝对目录写法仍可读取配置；新配置直接用 `file:///` URL。

嵌入式客户端可设置 `PVISOR_CACHE_READ_ONLY=true` 进入只读模式。只读客户端不能刷新或补齐共享池中的镜像名，仍可维护私有加速缓存。

### 发布本地镜像 {#publish-a-local-image}

在发布端配置 AWS 凭据和区域，并使用已有的 bucket。接收端使用发布回执中的 `source`。示例用 `jq` 提取该值。

```sh
pvisor pull alpine:3.20
SOURCE=$(pvisor push alpine:3.20 --to s3://your-bucket/releases | jq -er .source)
pvisor image pull alpine:published --from "$SOURCE"
```

将 `--to` 换为 `file:///ABSOLUTE/PATH` 可写入离线目录，然后在可访问该目录的主机直接用回执的 `file:///` URL 执行 `image pull --from`；不需要启动 HTTP 服务。也可以把同一发布包交给静态 HTTP(S) 镜像站。导入只读取描述符、OCI 配置和根元数据，文件内容按需读取。使用 `file:///` 导入后，目录必须保留，且不应原地修改。

S3 默认通过 HTTPS 使用 SigV4。发布需要 `s3:GetObject` 和 `s3:PutObject`，读取只需 `s3:GetObject`。认证由宿主环境凭据或工作负载角色提供。使用 S3 兼容服务时，配置 `AWS_ENDPOINT` 和区域。

将 `--to` 换为 `gs://BUCKET/PREFIX` 可发布到已有 GCS bucket。OpenDAL 使用宿主 Google Application Default Credentials；需要显式凭据文件时设置 `GOOGLE_APPLICATION_CREDENTIALS`。GCS 用对象 generation 执行条件创建与更新，不把它当成 S3 ETag。URL、镜像描述符和导入回执不放凭据。

### 共享池布局

已有共享池前缀仍可供 VM 缓存客户端和 Rust SDK 使用。共享池以可变 HEAD 记录解析名称。Push 生成带 `image.json` 描述符的独立发布包，使用返回的 source 导入。

```text
s3://your-bucket/pvisor-cache/
├── format.json
├── meta/<image-key>/
│   ├── identity.json
│   └── platforms/linux-amd64/
│       ├── HEAD.json
│       ├── revisions/<revision-hex>/
│       │   ├── manifest.json
│       │   ├── config.json
│       │   ├── root.bin
│       │   ├── index.bin
│       │   ├── objects.bin
│       │   └── COMMIT.json
│       └── uploads/<upload-id>/
│           ├── plan.json
│           └── progress.json
└── data/
    └── blobs/<first-two-hex>/<next-two-hex>/<encoded-blob-sha256>
```

写入者按目录自底向上聚合小文件帧，大文件帧按文件偏移独立聚合。索引定位压缩字节范围与 slot。跨 revision 只对完整编码 blob 去重，旧 HEAD 不决定布局。详见[打包规则](../design/experimental/image-pack/index.md#packing)。

### 存储布局与请求成本

每个二进制元数据对象最多 64 MiB，控制对象最多 1 MiB；最多 1,000,000 个条目与 2,000,000 个内容跨度，可能先达到对象大小上限。S3 兼容服务必须支持 Range GET、If-None-Match 条件创建与 If-Match 条件更新。

发布先写并校验全部共享内容、镜像元数据与 COMMIT，再 CAS 更新自身平台的 HEAD。同一旧 HEAD 只有一个发布者能提交；冲突明确失败，不盲目覆盖。损坏的已存在不可变对象也拒绝复用，不覆盖其他镜像正在使用的数据。失败会留下尚未引用的版本/对象。上传完成记录保留在 uploads，清理留给离线维护。

打开只获取控制对象、root.bin 与根目录 frame，不完整解析文件列表。lookup/readdir 按需取独立压缩索引 frame；V1 内容读取只取所需的压缩帧范围。索引 LRU 限制为 16 MiB 解码驻留，内容为 64 MiB／4,096 项；非池读取器可将编码 frame 持久保存到 <user-cache>/pvisor/cache-v2/objects/<location-hash>/。本地池普通读取没有持久 raw 镜像副本。本地缓存损坏后重新获取；远端缺失、损坏和鉴权失败明确报错，不填零、不改走 registry。

读写模式的可变 tag HEAD 在五分钟内复用；--refresh 由发布端显式更新。只读模式始终使用已发布 HEAD，无 registry 查询。每个任务固定返回的 image_handle，旧 revision 保留时仍可读取。IMAGE@sha256:… 是独立引用，需要另行发布其 meta；持有 tag 返回的 image_handle 已能固定那个版本。

S3 实际流量包括控制对象、元数据页和完整数据块。cache 接口返回的逻辑文件字节不能估算计费流量。自动 GC/配额尚未实现；删除 meta 或 data 前需要停用相关任务/发布并确认读者退出，不能按对象年龄删除共享内容。

## 代码布局

实现位于 `crates/pvisor/src/image/cache/`：

`pvisor-daemon serve --cache` 提供服务入口。运行时导出 Rust 客户端。

```text
cache/
├── mod.rs              # 公共入口与模块装配
├── protocol.rs         # 请求/响应类型、分帧、内容哈希
├── transport.rs        # Unix/TCP 端点、流、超时
├── client.rs           # 后端发现与校验后的请求
├── config.rs           # CLI/执行器共享的后端配置
├── storage.rs          # 文件系统和 S3 对象 I/O
├── portable.rs         # 直接存储格式、索引和范围读取
├── portable/           # 打包发布与回归测试
├── server.rs           # 认证、工作队列与统一镜像读取器
├── server/tests.rs     # 协议及 Server/直接存储一致性测试
├── source.rs           # 发布时受限地读取 OCI 解包源
├── source/             # 发布源元数据与完整性测试
├── backend.rs          # 入口无关的元数据、按块读取与有界缓存
├── direct.rs           # VM lower 元数据投影和 runner 接入
├── network.rs          # 隔离 runner 的固定镜像只读联网通道
├── lazy.rs             # host FUSE 适配器
├── lazy/
│   └── tests.rs        # 懒加载文件系统与缓存复用测试
└── progress.rs         # 镜像总量与加载/下载进度
```

`image/store.rs` 负责本地路径选择、临时导入和共享拉取锁。`image/oci.rs` 负责 registry 解析、已准备镜像记录、blob 校验和层解包。VM 本地准备、Docker 命令与缓存服务端共用 `LocalStore` 和 V2 池。外部调用方继续使用 `cache` 模块导出的 API；协议与传输的内部辅助项保持在缓存子系统内部。

## 用法

启动缓存服务后，通过 Rust 客户端发送 Prepare 请求。源码示例 `cache_probe` 接收 JSON 请求，也可按句柄和路径读取完整文件。它使用 `CacheClient::from_env()`。自己的诊断工具可使用 `Request::List`、`Request::Stat` 和 `Request::Read`。

```sh
pvisor-daemon serve --cache

# From a checkout, in a separate terminal on an amd64 host:
cargo run --locked -p pvisor --example cache_probe -- \
  '{"op":"prepare","image":"alpine:3.20","architecture":"amd64"}'
```

`PVISOR_CACHE_SERVER` 为客户端和服务端选择端点。服务端用 `pvisor-daemon serve --cache --cache-listen` 覆盖它。没有覆盖时端点为 `unix://<dirs::cache_dir()>/pvisor/cache.sock`：

- macOS：`~/Library/Caches/pvisor/cache.sock`
- Linux：`$XDG_CACHE_HOME/pvisor/cache.sock`，通常为
  `~/.cache/pvisor/cache.sock`

服务端接受 `--cache-image-store DIR` 或 `PVISOR_IMAGE_STORE` 指定本地镜像目录，与 Docker 和无守护进程的 VM Session 共用。它不会自动启动。

## VM 自动懒加载

未选择文件系统或 S3 后端时，当 `pvisor spawn --executor vm --rootfs image=IMAGE -- COMMAND` 准备 OCI 镜像时，它用一个两秒的 `ping` 握手探测默认 socket。有兼容的服务端在线时自动选择懒加载；socket 缺失或连接被拒（陈旧 socket）时使用同一个本地 V2 池准备并按需读取。认证、协议和超时错误会被报告，而不是静默绕过。

显式设置 `PVISOR_CACHE_SERVER` 时要求该服务可用。设置 `PVISOR_CACHE_SERVER=off` 强制使用本地 V2 池。显式 rootfs 目录和原生容器执行保持原有行为。

VM 客户端将不可变只读 lower 直接接入 virtio-fs 文件服务，保留现有的可写 upper，不建立中间宿主 FUSE 挂载。host 工具仍通过 FUSE 适配器访问同一后端。元数据按需获取，并在后端使用期间保留在内存中。`metadata_generation` 固定为 revision 的 COMMIT 摘要；socket/文件系统/S3 客户端可持久缓存校验过的属性和目录页，重建 OCI 解包目录不改变已发布 revision。socket/文件系统/S3 客户端将内容以 1 MiB 块缓存到 `<user-cache>/pvisor/blocks/<endpoint-hash>/<read-handle-hash>/`，
按文件/块为键。macOS 上 `<user-cache>` 为 `~/Library/Caches`；Linux 上为 `$XDG_CACHE_HOME`，通常为 `~/.cache`。此块缓存独立于 `--image-store` 和 `PVISOR_IMAGE_STORE`。小文件占用一个不填充的块；大文件只获取被访问的块。每个后端都会在按文件为键的内存缓存中保留已校验内容，上限 64 MiB 和 4096 块，FIFO 淘汰。热读只复制请求的切片，不重新打开或重新哈希磁盘块。内存未命中时，磁盘块会再次校验；磁盘损坏不会改变已校验并保留在内存中的字节。新块经校验和后原子发布，并通过文件锁在本地进程间共享；损坏的磁盘块会被重新获取。VM 维护私有元数据投影和稀疏占位文件，但 guest READ 使用后端内容，不读取空洞。内核预读可能获取相邻字节，copy-up 和文件摘要需要完整原文件，完整快照或自包含目录导出会补齐全树；普通按需读取不提取完整镜像。

直接后端在 VM 运行期间保持接入，结束后释放私有投影；host FUSE 的挂载则保持到使用者退出。缓存的块保留。服务失败后缓存数据仍可读取，但缺失的块会以 I/O 错误失败。摘要和端点在一次运行期间固定；运行中途不会回退到 registry。缓存端点/令牌会从隐式继承的 guest 环境变量中移除。

服务端在应答 `prepare` 前仍会完整准备未缓存的镜像，并在临时导入目录中生成索引和内容对象；成功或普通失败后删除该临时目录。这是客户端侧的懒加载，不是服务端的惰性 OCI 层解包。FUSE 适配器和现有 virtio-fs worker 目前同步处理请求：一次缓存未命中可能延迟无关的文件系统请求。不使用显式 vCPU 暂停。磁盘缓存配额/淘汰、原始 OCI xattr 和异步 virtio-fs 完成不在此实现中加入。

阻塞式 Rust 客户端直接使用已选后端。自动回退到本地池是 VM 镜像来源选择的一部分。

Unix socket 权限为 0600，并要求两端为同一有效用户。锁可防止两个服务端占用同一 socket；重启时会回收陈旧 socket，但普通文件、符号链接或活跃监听者绝不会被移除。请把 socket 放在由服务用户拥有的目录中。用 Ctrl-C 停止前台服务端可能留下陈旧 socket；无需手工清理。

对远程服务端，请在 SSH 隧道内使用带认证的 loopback TCP：

```sh
# 服务端：通过你的密钥管理/命令行设置一个强共享密钥。
export PVISOR_CACHE_TOKEN='YOUR_RANDOM_SECRET'
pvisor-daemon serve --cache --cache-listen tcp://127.0.0.1:7447

# 客户端机器上，保持该隧道运行：
ssh -N -L 7447:127.0.0.1:7447 your-server

# 客户端 shell，使用同一密钥：
export PVISOR_CACHE_TOKEN='YOUR_RANDOM_SECRET'
export PVISOR_CACHE_SERVER=tcp://127.0.0.1:7447
cargo run --locked -p pvisor --example cache_probe -- \
  '{"op":"prepare","image":"alpine:3.20","architecture":"amd64"}'
```

TCP 要求非空令牌，且只接受字面 loopback IP 端点。没有内置 TLS；请用 SSH 做传输加密。令牌授予所有缓存操作，包括准备新镜像，因此这是受信任的共享服务，不是公共多租户 API。若为 Unix 服务端配置了令牌，Unix 客户端也必须提供。

## 线格式

服务端和 VM bridge 只接受协议 2。客户端在实际请求前发送一次无副作用 Ping；被拒绝或遇到 EOF 时明确失败，不自动降级。顺序复用最多保留四条客户端连接；关闭连接复用仍发送 V2。存储格式 V2 与传输协议独立。帧为四字节大端长度和 UTF-8 JSON，JSON 上限 1 MiB；read 与 metadata 的成功响应之后携带声明长度的原始字节。传输体不压缩（metadata 对象自身可以压缩），必须校验长度与 SHA-256。

请求信封：

```json
{"version":2,"token":null,"request":{"op":"read","digest":"pvisor-v2:IMAGE_KEY:linux-amd64:REVISION","path":[101,116,99,47,111,115,45,114,101,108,101,97,115,101],"offset":0,"length":1048576}}
```

路径和目录名是 Unix 文件名字节的 JSON 数组，以保留非 UTF-8 文件名。路径相对于镜像根；空路径表示根目录。绝对路径、向上遍历和 NUL 会被拒绝。符号链接作为元数据返回，服务端路径解析绝不跟随它们。guest 文件系统遍历在 guest 树内解析符号链接。

| `op` | 字段 | 响应 `status` |
| --- | --- | --- |
| `ping` | 无 | `ready` |
| `prepare` | `image`、`architecture`（`amd64` 或 `arm64`）、可选 `refresh`（默认 false） | `prepared`：`digest`、`architecture`、`env`、`entrypoint`、`cmd`、可选 `totals`（`files`、`bytes`）、必填 `metadata_generation` 和 `image_handle` |
| `list` | `digest`、`path`、`offset`（初始为 0，之后使用 opaque continuation） | `entries`：已排序的 `names`、必填且对齐的 `metadata` 数组、`next_offset`（完成时为 null） |
| `stat` | `digest`、`path` | `metadata`：`kind`、`size`、`mode`、`uid`、`gid`、`inode`、`nlink`、`mtime`、`mtime_nsec`、`target` |
| `read` | `digest`、`path`、`offset`（字节偏移）、`length`（1..1048576） | `data`：`length`、`sha256`，后接原始字节 |
| `open` | `handle`, `architecture` | `prepared` |
| `metadata` | `handle`, `object_name`, `offset`, `length` | `data` + bytes |

`prepare` 为客户端架构请求 Linux 镜像，与服务端架构无关。已发布的 V2 HEAD 记录平台摘要和启动配置，不依赖持久 raw 准备记录。本地池标签可离线使用到显式刷新或替换；可写文件系统/S3 后端对可变 HEAD 复用五分钟，对不可变摘要 HEAD 则复用到 refresh。设 `refresh: true` 强制 registry 解析。导入失败保留旧 HEAD，并清理临时目录。registry 连接超时为 10 秒，阻塞请求及 body 读取超时为 30 秒。瞬态 blob 传输失败用新临时文件重试一次；429/503 按不超过十秒的 Retry-After 重试一次；过期标签不会静默回退到陈旧数据。本地引用/平台锁串行化本地导入；远程 HEAD 通过条件更新发布。`read`、`stat`、`list` 要求已发布的不可变句柄，不隐式拉取镜像。

目录页包含与 `stat` 相同的属性，避免为每个子项单独请求。页最多 256 个条目，并会缩小以适应 JSON
帧上限（含长字节数组名和符号链接目标）。目录属性为必填字段，仅包含名字的响应会被拒绝。持久化的页在多次挂载间保留其属性。

`kind` 为 `file`、`directory`、`symlink` 或 `special`。`mode` 含 Unix 类型／权限位，`target` 为链接字节或 null。保留已准备源的 ownership/mode override；不提供任意 xattr 或跨服务端可移植 inode ID。只有常规文件可读，短读（包括零字节）表示 EOF。客户端校验长度与 SHA-256 后才能缓存。哈希检测损坏，不构成对不可信服务端的独立证明。

错误是如下帧：

```json
{"status":"error","code":"not_found","message":"..."}
```

错误码有 `not_found`、`permission_denied` 和 `request_failed`（含无效参数、不支持的协议版本和认证失败）。分帧错误也可能导致断开。客户端必须把提前关闭、截断内容体或坏校验和视为失败，绝不当成缺失文件或零填充内容。错误消息用于说明，不保证机器稳定。

服务端读取使用与文件系统/S3 相同的有界镜像、元数据页和内容对象缓存。OCI 源目录检查及其目录缓存用于发布，不再用于处理 guest 文件请求。

服务端最多接纳 64 个连接，分帧／空闲等待与 16 个文件 worker 分离；文件队列上限 16。准备任务有独立的两个 worker 与 16 项队列。饱和时返回 busy 或关闭未接纳连接。服务端空闲超时五秒，客户端池四秒过期。长准备任务可能在客户端断开后继续，关闭不会优雅取消独立 OCI 下载；自动磁盘配额与 GC 未实现。

不存在 vCPU 暂停／恢复消息。VM 文件服务在隔离 runner 中处理请求，本地文件与 Unix socket 缓存直接访问。Linux TCP/S3 经私有 Unix socket 委托宿主访问进程，只允许绑定镜像的 stat/list/read 及白名单 metadata，禁止 prepare、open 与其他镜像。凭据留在隐藏 owner，进程随 VM teardown 终止。host FUSE 在宿主取内容。

## 验证后端

`mise run test -- pvisor pvisor-cli` 包含服务器兼容性、独立 Rust 缓存客户端读写文件系统/S3、只读权限、损坏拒绝与配置优先级检查。S3 本地夹具独立验算 SigV4（包括临时会话令牌），不使用真实账号、公网请求或外部 daemon。

有 KVM 与静态 musl guest target 的 Linux x86_64 主机可以执行真实 VM 验收：

```sh
cargo nextest run --locked -p pvisor-cli --test cache_backends --run-ignored only \
  -E 'test(vm_reads_daemonless_filesystem_and_s3_images_without_projecting_storage_credentials)'
```

此验收从两个后端启动 VM，删除发布端暂存目录后校验 guest 文件内容，并验证继承宿主环境时不会泄漏存储凭据。
