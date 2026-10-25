# 共享镜像缓存与存储后端

`pvisor cache` 通过服务器、文件系统或 S3 复用 OCI 镜像内容。服务器模式的 `pvisor-daemon serve --cache` 自动准备 OCI 镜像并生成分页索引和内容对象，存放在 `<image-store>/pool-v2/`。三个后端共用不可变 revision、对象格式和镜像读取器。客户端使用 `image_handle` 查询，不接收宿主路径；文件查询不访问 registry。

## 本地镜像目录 {#local-image-store}

Docker 兼容命令、未选择远程后端的 spawn 和 cache server 共用一个本地镜像目录。macOS 默认是 `~/Library/Application Support/pvisor/docker/images/`；Linux 默认是 `$XDG_DATA_HOME/pvisor/docker/images/`，通常为 `~/.local/share/pvisor/docker/images/`。这里保留被任务引用的镜像 revision，使用持久数据目录。

显式 Job/cache 镜像目录优先，其次是 `PVISOR_IMAGE_STORE`。只设置 `PVISOR_DOCKER_HOME` 时，所有本地入口使用其 `images/`，保留隔离存储语义；嵌入式 `Store::open(root)` 固定使用 `root/images`，不读取环境默认值。容器状态、Job 记录和可写层仍各自保存。本地镜像根目录及控制子目录必须由当前用户拥有、权限私有（0700）且不是符号链接；不兼容的已有目录会报错，不自动修改权限。

```text
<image-store>/
├── configs/<config-digest>.json
├── pool-v2/
│   ├── format.json
│   ├── pool-format.json
│   ├── meta/<image-key>/...
│   └── data/{packs,sha256}/...
├── staging/.import-*/
├── locks/
└── projections/
```

`staging` 中的下载／解包目录在普通成功或失败后删除，进程崩溃可能留下残余。`configs` 保存按摘要校验的原始 OCI 配置；`pool-v2` 是唯一的新本地镜像发布位置。Docker pull 仍查询 registry，但可复用相同 digest 的共享池内容。build 的各个标签也发布到 V2，每个标签独立提交，不提供跨标签事务。`rmi` 只移除容器前端 catalog 引用，不回收共享内容；活动和已停止任务保留固定 revision。

本地池标签持续可离线使用，直到显式 pull/refresh 或新的发布替换它。五分钟的标签自动刷新窗口适用于可写的文件系统/S3 后端，不适用于本地池。

只接受当前 V2 镜像记录和池格式。目录式 rootfs 记录、缺少 platform/binding 的记录和旧缓存布局会被拒绝，不提供迁移或兼容读取器。先通过旧版本停止并清理旧容器，再在新存储中重新 pull/build 并重建容器；已有文件不自动删除。

## 选择后端

服务器、文件系统、S3 与本地 pool 使用[共享镜像缓存 V2](../design/lazy-image-v2.md)：独立镜像元数据、压缩目录 radix 索引与共享内容池。V1 存储实现已删除，旧缓存需要从 OCI 重新发布到空目录／前缀；不会自动删除旧数据。

服务器、文件系统和 S3 使用同一套 `prepare/list/stat/read` 接口，VM 也使用相同配置。文件系统和 S3 是直接存储后端，使用它们不需要启动 `pvisor-daemon serve --cache`。

| 后端 | 配置 | 适用场景 |
|---|---|---|
| `server`（默认） | `PVISOR_CACHE_SERVER=unix://...` 或 loopback `tcp://...` | 服务同一台机器的 V2 镜像池，或已有集中服务 |
| `filesystem` | `PVISOR_CACHE_BACKEND=filesystem` 与 `PVISOR_CACHE_LOCATION=/absolute/directory` | 单机多个进程，或共享盘/NFS 上的缓存 |
| `s3` | `PVISOR_CACHE_BACKEND=s3` 与 `PVISOR_CACHE_LOCATION=s3://BUCKET/PREFIX` | 多机器通过对象存储共享镜像，无需维护缓存服务 |

命令行 `--backend`、`--location`、`--image-store` 优先于环境变量，在子命令前后都可使用。`--read-only` 或 `PVISOR_CACHE_READ_ONLY=true` 让共享后端只接受读取：准备未发布镜像和 `--refresh` 会明确失败，不访问 registry，不尝试写入后端。只读客户端仍可写自己的本地加速缓存。

### 文件系统：发布一次，独立进程读取

```sh
pvisor cache --backend filesystem --location /mnt/pvisor-cache \
  --image-store /tmp/pvisor-publish publish alpine:latest
pvisor cache --backend filesystem --location /mnt/pvisor-cache \
  --read-only prepare alpine:latest
pvisor cache --backend filesystem --location /mnt/pvisor-cache \
  --read-only read pvisor-v2:YOUR_IMAGE_KEY:linux-amd64:YOUR_REVISION etc/os-release
```

`--location` 是新的共享缓存目录，`--image-store` 是发布时用来拉取/解包 OCI 镜像的本地暂存目录；两者不同。也可以复用现有 `PVISOR_IMAGE_STORE`。发布完成后读取不依赖暂存目录，CI 可把它设为任务临时目录。共享目录中的对象默认私有，跨 UID 使用共享盘时需由存储管理员设置相应读取权限；只读模式不会创建缺失的共享目录。

### S3：发布端有写权限，使用方只读

```sh
export AWS_DEFAULT_REGION=ap-southeast-1
# Supply AWS credentials through environment variables or workload roles.
export PVISOR_CACHE_BACKEND=s3
export PVISOR_CACHE_LOCATION=s3://your-bucket/pvisor-cache
pvisor cache --image-store /tmp/pvisor-publish publish alpine:latest

# Native cache consumers only need GetObject access to this prefix.
export PVISOR_CACHE_READ_ONLY=true
pvisor cache prepare alpine:latest
pvisor cache read pvisor-v2:YOUR_IMAGE_KEY:linux-amd64:YOUR_REVISION etc/os-release
pvisor spawn --executor vm --rootfs image=alpine:latest -- /bin/sh
```

桶需要预先建立；pVisor 不创建桶。publish 和读写 prepare 都需要 s3:GetObject 与 s3:PutObject：读取 HEAD 的 CAS 令牌，并验证已存在的不可变对象。读取节点只需要 `s3:GetObject`，正常读写不需要 `ListBucket` 或 `DeleteObject`。S3 存储连接由宿主负责，新的缓存配置及其隐式 `AWS_*` 凭据不会投影到 guest；显式传给工作负载的环境变量仍由调用方决定。

S3 使用 SigV4，默认 HTTPS。凭据支持 `AWS_ACCESS_KEY_ID`、`AWS_SECRET_ACCESS_KEY`、可选 `AWS_SESSION_TOKEN`，以及 EC2、ECS、Web Identity 工作负载角色；由 [object_store 的 S3 provider](https://docs.rs/object_store/0.13.2/object_store/aws/struct.AmazonS3Builder.html) 管理获取与续期。当前不直接读取 `~/.aws` profile/SSO 文件，可向进程提供导出的环境凭据或使用工作负载角色。区域用 `AWS_DEFAULT_REGION` 或 `AWS_REGION`。S3 兼容服务可设置 `AWS_ENDPOINT`（或 `AWS_ENDPOINT_URL_S3`）；下面的 HTTP 配置仅用于本机测试服务。

```sh
export AWS_ENDPOINT=http://127.0.0.1:9000
export AWS_ALLOW_HTTP=true
export AWS_ACCESS_KEY_ID=YOUR_ACCESS_KEY
export AWS_SECRET_ACCESS_KEY=YOUR_SECRET_KEY
export PVISOR_CACHE_BACKEND=s3
export PVISOR_CACHE_LOCATION=s3://your-bucket/pvisor-cache
pvisor cache prepare alpine:latest
```

### 镜像拆分与上传工具

`pvisor cache publish IMAGE` 是显式发布入口：从 registry 拉取所选平台的 OCI manifest 和层，在本地应用层与 whiteout，遍历合并后的镜像文件，再上传文件索引和内容块。已有的本地 OCI 暂存可通过 `--image-store` 或 `PVISOR_IMAGE_STORE` 复用；上传后可以删除暂存目录。

```sh
PVISOR_CACHE_READ_ONLY=false pvisor cache publish alpine:latest \
  --backend s3 --location s3://your-bucket/pvisor-cache \
  --architecture amd64 --image-store /tmp/pvisor-publish
```

`--architecture` 支持 `amd64`、`arm64`，默认取宿主架构。`--refresh` 重新查询 registry；不指定时可使用本地仍有效的已准备镜像记录。`publish` 总会执行拆分和上传，不根据远端 tag 记录跳过；已存在的不可变内容块通过条件创建复用，缺失的块会重新上传。只读模式和服务器后端不接受发布。命令成功时输出 JSON，包含 manifest 摘要、架构、COMMIT 摘要 `metadata_generation` 以及文件数/逻辑字节总量 `totals`；同时返回 image_handle=pvisor-v2:<image-key>:<platform>:<revision-hex>，list/stat/read 以这个句柄查询；digest 仅记录来源 manifest。

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
    ├── packs/<first-two-hex>/<next-two-hex>/<encoded-blob-sha256>
    └── sha256/<first-two-hex>/<next-two-hex>/<plaintext-chunk-sha256>
```

≤64 KiB 小文件按目录聚合为不可变压缩 pack，索引记录 blob/frame/slot，偏移保存在数据 frame 的 slot 表；大文件按自身 offset 0 切成至多 1 MiB 的内容块。数据对象不含文件系统元数据，旧 HEAD 中的内容位置可复用于 rename、移动和 chmod。部分重叠的小文件集合不保证跨镜像逐文件物理去重。路径与符号链接保留原始字节，硬链接保持 inode 身份，不新增 xattr。

### 存储布局与请求成本

每个二进制元数据对象最多 64 MiB，控制对象最多 1 MiB；最多 1,000,000 个条目与 2,000,000 个内容跨度，可能先达到对象大小上限。S3 兼容服务必须支持 Range GET、If-None-Match 条件创建与 If-Match 条件更新。

发布先写并校验全部共享内容、镜像元数据与 COMMIT，再 CAS 更新自身平台的 HEAD。同一旧 HEAD 只有一个发布者能提交；冲突明确失败，不盲目覆盖。损坏的已存在不可变对象也拒绝复用，不覆盖其他镜像正在使用的数据。失败会留下尚未引用的版本/对象。上传完成记录保留在 uploads，清理留给离线维护。

打开只获取控制对象、root.bin 与根目录 frame，不完整解析文件列表。lookup/readdir 按需取独立压缩索引 frame，read 获取相关布局与数据 frame。索引 LRU 限制为 16 MiB 解码驻留，内容为 64 MiB／4,096 项；非池读取器可将编码 frame 持久保存到 <user-cache>/pvisor/cache-v2/objects/<location-hash>/。本地池普通读取没有持久 raw 镜像副本。本地缓存损坏后重新获取；远端缺失、损坏和鉴权失败明确报错，不填零、不改走 registry。

读写模式的可变 tag HEAD 在五分钟内复用；--refresh 由发布端显式更新。只读模式始终使用已发布 HEAD，无 registry 查询。每个任务固定返回的 image_handle，旧 revision 保留时仍可读取。IMAGE@sha256:… 是独立引用，需要另行发布其 meta；持有 tag 返回的 image_handle 已能固定那个版本。

S3 实际流量包括控制对象、元数据页和完整数据块。现有 TUI Transferred 只统计 cache 接口返回的逻辑文件字节，不能估算计费流量。自动 GC/配额尚未实现；删除 meta 或 data 前需要停用相关任务/发布并确认读者退出，不能按对象年龄删除共享内容。

## 代码布局

实现位于 `crates/pvisor/src/image/cache/`：

缓存命令解析位于 `crates/pvisor-cli/src/cli/cache.rs`。`pvisor-daemon serve --cache` 拥有服务入口；存储和协议实现保留在 runtime。

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

```sh
# 终端 1：前台服务端，使用默认的按用户 Unix socket 与 OCI 存储
pvisor-daemon serve --cache

# 终端 2：使用同一个默认 socket
pvisor cache prepare alpine:latest
# 即使处于五分钟 tag 缓存窗口内也强制刷新 registry：
pvisor cache prepare alpine:latest --refresh
# 从 JSON 结果复制 image_handle：
pvisor cache list pvisor-v2:YOUR_IMAGE_KEY:linux-amd64:YOUR_REVISION
pvisor cache stat pvisor-v2:YOUR_IMAGE_KEY:linux-amd64:YOUR_REVISION etc/os-release
pvisor cache read pvisor-v2:YOUR_IMAGE_KEY:linux-amd64:YOUR_REVISION etc/os-release
```

`PVISOR_CACHE_SERVER` 为客户端和服务端选择端点。服务端用 `pvisor-daemon serve --cache --cache-listen` 覆盖它。没有覆盖时端点为 `unix://<dirs::cache_dir()>/pvisor/cache.sock`：

- macOS：`~/Library/Caches/pvisor/cache.sock`
- Linux：`$XDG_CACHE_HOME/pvisor/cache.sock`，通常为
  `~/.cache/pvisor/cache.sock`

服务端接受 `--cache-image-store DIR` 或 `PVISOR_IMAGE_STORE` 指定本地镜像目录，与 Docker 和无守护进程的 VM Job 共用。它不会自动启动。

## VM 自动懒加载

未选择文件系统或 S3 后端时，当 `pvisor spawn --executor vm --rootfs image=IMAGE -- COMMAND` 准备 OCI 镜像时，它用一个两秒的 `ping` 握手探测默认 socket。有兼容的服务端在线时自动选择懒加载；socket 缺失或连接被拒（陈旧 socket）时使用同一个本地 V2 池准备并按需读取。认证、协议和超时错误会被报告，而不是静默绕过。

显式设置 `PVISOR_CACHE_SERVER` 时要求该服务可用。设置 `PVISOR_CACHE_SERVER=off` 强制使用本地 V2 池。显式 rootfs 目录和原生容器执行保持原有行为。

VM 客户端将不可变只读 lower 直接接入 virtio-fs 文件服务，保留现有的可写 upper，不建立中间宿主 FUSE 挂载。host 工具仍通过 FUSE 适配器访问同一后端。元数据按需获取，并在后端使用期间保留在内存中。`metadata_generation` 固定为 revision 的 COMMIT 摘要；socket/文件系统/S3 客户端可持久缓存校验过的属性和目录页，重建 OCI 解包目录不改变已发布 revision。socket/文件系统/S3 客户端将内容以 1 MiB 块缓存到 `<user-cache>/pvisor/blocks/<endpoint-hash>/<read-handle-hash>/`，
按文件/块为键。macOS 上 `<user-cache>` 为 `~/Library/Caches`；Linux 上为 `$XDG_CACHE_HOME`，通常为 `~/.cache`。此块缓存独立于 `--image-store` 和 `PVISOR_IMAGE_STORE`。小文件占用一个不填充的块；大文件只获取被访问的块。每个后端都会在按文件为键的内存缓存中保留已校验内容，上限 64 MiB 和 4096 块，FIFO 淘汰。热读只复制请求的切片，不重新打开或重新哈希磁盘块。内存未命中时，磁盘块会再次校验；磁盘损坏不会改变已校验并保留在内存中的字节。新块经校验和后原子发布，并通过文件锁在本地进程间共享；损坏的磁盘块会被重新获取。VM 维护私有元数据投影和稀疏占位文件，但 guest READ 使用后端内容，不读取空洞。内核预读可能获取相邻字节，copy-up 和文件摘要需要完整原文件，完整快照或自包含目录导出会补齐全树；普通按需读取不提取完整镜像。

直接后端在 VM 运行期间保持接入，结束后释放私有投影；host FUSE 的挂载则保持到使用者退出。缓存的块保留。服务失败后缓存数据仍可读取，但缺失的块会以 I/O 错误失败。摘要和端点在一次运行期间固定；运行中途不会回退到 registry。缓存端点/令牌会从隐式继承的 guest 环境变量中移除。

服务端在应答 `prepare` 前仍会完整准备未缓存的镜像，并在临时导入目录中生成索引和内容对象；成功或普通失败后删除该临时目录。这是客户端侧的懒加载，不是服务端的惰性 OCI 层解包。FUSE 适配器和现有 virtio-fs worker 目前同步处理请求：一次缓存未命中可能延迟无关的文件系统请求。不使用显式 vCPU 暂停。磁盘缓存配额/淘汰、原始 OCI xattr 和异步 virtio-fs 完成不在此实现中加入。

公共 Rust 客户端是 `pvisor::cache::CacheClient::from_env()`，它是阻塞式的。`pvisor cache prepare/list/stat/read` 使用显式后端配置，不使用 VM 默认 socket 探测的本地回退策略。

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
pvisor cache prepare alpine:latest
```

TCP 要求非空令牌，且只接受字面 loopback IP 端点。没有内置 TLS；请用 SSH 做传输加密。令牌授予所有缓存操作，包括准备新镜像，因此这是受信任的共享服务，不是公共多租户 API。若为 Unix 服务端配置了令牌，Unix 客户端也必须提供。

## TUI 下载统计

使用 `--tui` 时，懒加载镜像运行显示三组，各带文件数和大小：
`Cached`（本地缓存读）、`Transferred`（从镜像服务端接收的内容，无论本地或远程）和 `Total`（整个镜像）。窄状态栏把它们缩写为 `C / X / T`；Overview 显示全部三行及精确字节数。Log 面板记录每次已校验的块传输及其文件路径、字节数和累计运行总量。状态栏和 Overview 还显示本地缓存读：不同文件路径数，以及从已校验磁盘或内存缓存提供的累计字节数，包括重复读取。只统计请求的切片，不统计内部读取的整个 1 MiB 块。这些与下载分开；它们不包含由宿主或 guest 内核页缓存满足的读取。每个文件的首次本地缓存命中在 Log 中显示为 `no download`。目录列举只获取元数据，不计为内容读取。镜像启动与 I/O 诊断也进入该面板而不是 guest 终端；没有 TUI 时它们走 stderr。下载统计本次运行从缓存服务端接收的已校验内容，不含本地缓存命中、协议元数据和 guest 网络流量。部分下载的文件只计一次；重复传输会再次累加字节。因此一次热运行可能显示零下载，即使它使用了镜像。

总量统计服务端已解包镜像中的常规文件路径及其未压缩逻辑大小，包括空文件和每个硬链接名。目录和符号链接不计。这些总量在仅元数据扫描后按 manifest 摘要缓存。它们描述整个镜像，不是压缩后的 OCI 层大小。缺少总量时显示 `?`。

## 线格式

协议 1 每连接一次交换；协议 2 经无副作用 Ping 协商后允许顺序复用，客户端最多保留四条连接。存储格式 V2 与传输协议版本独立。帧为四字节大端长度和 UTF-8 JSON，JSON 上限 1 MiB；read 与 metadata 的成功响应之后携带声明长度的原始字节。传输体不压缩（metadata 对象自身可以压缩），必须校验长度与 SHA-256。

请求信封：

```json
{"version":1,"token":null,"request":{"op":"read","digest":"pvisor-v2:IMAGE_KEY:linux-amd64:REVISION","path":[101,116,99,47,111,115,45,114,101,108,101,97,115,101],"offset":0,"length":1048576}}
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

`prepare` 为客户端架构请求 Linux 镜像，与服务端架构无关。成功的已准备镜像记录持久化到
`<image-store>/metadata/prepared-v1/`，包含平台摘要和启动配置。可变 tag 复用记录五分钟；不可变摘要记录在其解包 root 存在期间不过期。`pvisor cache prepare IMAGE
--refresh`（协议 `refresh: true`）强制 registry 解析。刷新失败会返回错误并保留之前的记录；registry 请求有
10 秒连接超时和 300 秒总超时。过期的 tag 不会静默回退到陈旧数据。缺失/损坏的记录或缺失 root 会重新准备。按引用和架构的锁覆盖解析与准备，
因此并发请求会复查并复用首个成功结果。准备可能填充一个未缓存镜像，并保留现有的按摘要解包锁。`read`、`stat` 和
`list` 要求已发布的不可变 image_handle；它们绝不隐式拉取镜像。

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

`mise run test -- pvisor pvisor-cli` 包含服务器兼容性、文件系统/S3 独立 CLI 读写、只读权限、损坏拒绝与配置优先级检查。S3 本地夹具独立验算 SigV4（包括临时会话令牌），不使用真实账号、公网请求或外部 daemon。

有 KVM 与静态 musl guest target 的 Linux x86_64 主机可以执行真实 VM 验收：

```sh
cargo nextest run --locked -p pvisor-cli --test cache_backends --run-ignored only
```

此验收从两个后端启动 VM，删除发布端暂存目录后校验 guest 文件内容，并验证继承宿主环境时不会泄漏存储凭据。
