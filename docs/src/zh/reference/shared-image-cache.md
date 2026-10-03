# 共享镜像缓存与存储后端

`pvisor cache` 通过服务器、文件系统或 S3 复用 OCI 镜像内容。服务器模式的 `pvisor cache serve` 把已有 OCI 存储暴露为只读文件服务。客户端不会收到宿主文件系统路径。镜像在服务端准备一次，之后以其解析后的平台 manifest 的 SHA-256 摘要寻址。现有存储负责 blob 校验、层应用和白障（whiteout）处理。文件查询不访问 registry。

## 选择后端

目录树、对象字段、文件路径到内容块的映射和发布一致性见[共享镜像缓存存储格式 v1](../design/shared-image-cache-storage.md)，其中包含可直接读取的完整示例对象。

后续布局设计改为[每个镜像独立 meta、共享分片 data 的 v2 格式](../design/shared-image-cache-storage-v2.md)。该设计尚未接入发布器/读取器，下面的命令仍使用 v1。

服务器、文件系统和 S3 使用同一套 `prepare/list/stat/read` 接口，VM 也使用相同配置。文件系统和 S3 是直接存储后端，使用它们不需要启动 `cache serve`。

| 后端 | 配置 | 适用场景 |
|---|---|---|
| `server`（默认） | `PVISOR_CACHE_SERVER=unix://...` 或 loopback `tcp://...` | 复用同一台机器的已解包 OCI 存储，或已有集中服务 |
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
  --read-only read sha256:YOUR_MANIFEST_DIGEST etc/os-release
```

`--location` 是新的共享缓存目录，`--image-store` 是发布时用来拉取/解包 OCI 镜像的本地暂存目录；两者不同。也可以复用现有 `PVISOR_IMAGE_STORE`。发布完成后读取不依赖暂存目录，CI 可把它设为任务临时目录。共享目录中的对象默认私有，跨 UID 使用共享盘时需由存储管理员设置相应读取权限；只读模式不会创建缺失的共享目录。

### S3：发布端有写权限，工作节点只读

```sh
export AWS_DEFAULT_REGION=ap-southeast-1
# Supply AWS credentials through environment variables or workload roles.
export PVISOR_CACHE_BACKEND=s3
export PVISOR_CACHE_LOCATION=s3://your-bucket/pvisor-cache
pvisor cache --image-store /tmp/pvisor-publish publish alpine:latest

# Workers only need GetObject access to this prefix.
export PVISOR_CACHE_READ_ONLY=true
pvisor cache prepare alpine:latest
pvisor cache read sha256:YOUR_MANIFEST_DIGEST etc/os-release
pvisor run --executor vm --rootfs image=alpine:latest -- /bin/sh
```

桶需要预先建立；pVisor 不创建桶。`publish` 向该前缀上传对象，需要 `s3:PutObject`；读写模式的 `prepare` 先查询缓存、未命中时发布，因此还需要 `s3:GetObject`。读取节点只需要 `s3:GetObject`，正常读写不需要 `ListBucket` 或 `DeleteObject`。S3 存储连接由宿主负责，新的缓存配置及其隐式 `AWS_*` 凭据不会投影到 guest；显式传给工作负载的环境变量仍由调用方决定。

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

`--architecture` 支持 `amd64`、`arm64`，默认取宿主架构。`--refresh` 重新查询 registry；不指定时可使用本地仍有效的已准备镜像记录。`publish` 总会执行拆分和上传，不根据远端 tag 记录跳过；已存在的不可变内容块通过条件创建复用，缺失的块会重新上传。只读模式和服务器后端不接受发布。命令成功时输出 JSON，包含 manifest 摘要、架构、索引摘要 `metadata_generation` 以及文件数/逻辑字节总量 `totals`；客户端随后通过这些索引按路径读取文件。

```text
s3://your-bucket/pvisor-cache/
└── v1/
    ├── format
    ├── refs/<reference-and-architecture-hash>.json
    ├── images/<manifest-sha256>.json
    ├── indexes/<index-sha256>.json
    └── blobs/<content-sha256>
```

这里的文件存储结构由索引中的文件路径、权限、硬链接、符号链接和内容片段共同表示。S3 中的内容对象是至多 1 MiB 的块；一个大文件可以使用多个对象，多个小文件也可以共享同一个对象。文件路径由索引映射到内容块，读取端无需重新下载或解包 OCI 层。

### 存储布局与请求成本

直接存储后端的单镜像索引上限为 64 MiB，最多 200,000 个路径和 500,000 个内容片段；超过上限会明确失败。兼容的对象服务需要支持原子 PUT 和 `If-None-Match` 条件创建。

两个直接存储后端共享 `v1/` 格式：`refs/` 保存按镜像引用和架构的记录，`images/` 保存 manifest 摘要到索引的指针，`indexes/` 保存不可变的元数据和文件内容片段索引，`blobs/` 保存 SHA-256 标识的内容块。文件名与符号链接使用 Unix 原始字节，硬链接保持相同 inode 身份，文件模式和 Linux 身份沿用 OCI 提取规则。

发布把小文件合并进至多 1 MiB 的块；大文件可以跨块。块按内容去重，先写全部块和索引，最后原子发布引用。并发发布使用不可变对象的条件创建，不向读者暴露正在写入的对象。失败可能留下未引用的块，但不会发布指向未完成上传的引用。

读取先获取引用和索引，随后只读请求范围所在的块；目录查询只访问索引。单个客户端保持有界内存缓存，本地 `<user-cache>/pvisor/cache-v1/objects/<location-hash>/` 还持久复用经过摘要验证的索引与打包块。损坏的本地对象会重新获取；损坏、缺失或被拒绝的远端对象使操作失败，不填零或静默改走 registry。底层块缓存之外，VM 的文件块和元数据缓存也继续生效。

可变 tag 的发布记录在读写模式下缓存五分钟；`--refresh` 由发布端显式更新。只读模式使用已发布记录，即使超过五分钟也不去 registry；需要更新时由发布端刷新，任务随后固定到返回的 manifest 摘要。使用 `IMAGE@sha256:...` 可以固定版本。

S3 的实际传输包括索引和完整打包块，小文件读取可能获取邻近文件字节。现有 TUI `Transferred` 计数是 cache 接口返回的逻辑文件字节，不是 S3 GET 次数或计费流量；不要用它估算对象存储账单。共享块的自动 GC/配额暂未加入；运维可按独立前缀分组保存和退役，确认没有任务使用旧前缀后整体清理。不能单独按块年龄删除仍被新引用使用的内容。

## 代码布局

实现位于 `crates/pvisor/src/image/cache/`：

```text
cache/
├── mod.rs              # 公共入口与模块装配
├── cli.rs              # pvisor cache 子命令
├── protocol.rs         # 请求/响应类型、分帧、内容哈希
├── transport.rs        # Unix/TCP 端点、流、超时
├── client.rs           # 后端发现与校验后的请求
├── config.rs           # CLI/执行器共享的后端配置
├── storage.rs          # 文件系统和 S3 对象 I/O
├── portable.rs         # 直接存储格式、索引和范围读取
├── portable/           # 打包发布与回归测试
├── server.rs           # 认证、工作队列、受限文件访问
├── server/
│   ├── metadata.rs     # 服务端元数据与目录 LRU 缓存
│   └── tests.rs        # 协议、受限访问与客户端/服务端测试
├── lazy.rs             # FUSE 挂载、块缓存与客户端元数据缓存
├── lazy/
│   └── tests.rs        # 懒加载文件系统与缓存复用测试
└── progress.rs         # 镜像总量与加载/下载进度
```

`image/oci.rs` 负责 registry 解析、已准备镜像记录、blob 校验和层解包。本地加载与缓存服务端共用 `ImageStore`。外部调用方继续使用 `cache` 模块导出的 API；协议与传输的内部辅助项保持在缓存子系统内部。

## 用法

```sh
# 终端 1：前台服务端，使用默认的按用户 Unix socket 与 OCI 存储
pvisor cache serve

# 终端 2：使用同一个默认 socket
pvisor cache prepare alpine:latest
# 即使处于五分钟 tag 缓存窗口内也强制刷新 registry：
pvisor cache prepare alpine:latest --refresh
# 从 JSON 结果复制 digest：
pvisor cache list sha256:YOUR_MANIFEST_DIGEST
pvisor cache stat sha256:YOUR_MANIFEST_DIGEST etc/os-release
pvisor cache read sha256:YOUR_MANIFEST_DIGEST etc/os-release
```

`PVISOR_CACHE_SERVER` 为客户端和服务端选择端点。服务端用 `cache serve --listen` 覆盖它。没有覆盖时端点为 `unix://<dirs::cache_dir()>/pvisor/cache.sock`：

- macOS：`~/Library/Caches/pvisor/cache.sock`
- Linux：`$XDG_CACHE_HOME/pvisor/cache.sock`，通常为
  `~/.cache/pvisor/cache.sock`

服务端接受 `--image-store DIR` 或 `PVISOR_IMAGE_STORE` 指定已有的 OCI 存储。它不会自动启动。

## VM 自动懒加载

未选择文件系统或 S3 后端时，当 `pvisor run --executor vm --rootfs image=IMAGE -- COMMAND` 准备 OCI 镜像时，它用一个两秒的 `ping` 握手探测默认 socket。有兼容的服务端在线时自动选择懒加载；socket 缺失或连接被拒（陈旧 socket）时走现有的本地 OCI 准备路径。认证、协议和超时错误会被报告，而不是静默绕过。

显式设置 `PVISOR_CACHE_SERVER` 时要求该服务可用。设置 `PVISOR_CACHE_SERVER=off` 强制本地准备。显式 rootfs 目录和原生容器执行保持原有行为。

客户端挂载一个不可变的只读 FUSE lower（macOS 用 macFUSE FSKit；Linux 用 FUSE），保留现有的 VM 可写 upper。元数据按需获取，并在挂载期间保留在内存中。当服务端通告 `metadata_generation` 时，校验过的 stat 响应（包括缺失路径）和目录页也会持久化到
`<user-cache>/pvisor/metadata/v1/<endpoint-hash>/<manifest-digest>/<generation-hash>/`。
它们在 VM 退出后仍然存在；损坏条目会被重新获取。不提供 generation 的旧服务端保持此前的仅内存行为。generation 包含服务端根目录的身份和变更时间，因此重建解包后的 root 会使包含旧宿主 inode 号的元数据失效。已准备的 root 必须保持不可变；不支持在其下就地修改。内容以 1 MiB 块获取到 `<user-cache>/pvisor/blocks/<endpoint-hash>/<manifest-digest>/`，
按文件/块为键。macOS 上 `<user-cache>` 为 `~/Library/Caches`；Linux 上为 `$XDG_CACHE_HOME`，通常为 `~/.cache`。此块缓存独立于 `--image-store` 和 `PVISOR_IMAGE_STORE`。小文件占用一个不填充的块；大文件只获取被访问的块。每次挂载都会在按文件为键的内存缓存中保留已校验内容，上限 64 MiB 和 4096 块，FIFO 淘汰。热读只复制请求的切片，不重新打开或重新哈希磁盘块。内存未命中时，磁盘块会再次校验；磁盘损坏不会改变已校验并保留在内存中的字节。新块经校验和后原子发布，并通过文件锁在本地进程间共享；损坏的磁盘块会被重新获取。不暴露稀疏占位文件。内核正常预读可能获取相邻字节，copy-up 可能读取整个文件。客户端不提取完整镜像。

FUSE 挂载在 VM 运行结束前一直存在，随后卸载；缓存的块保留。服务失败后缓存数据仍可读取，但缺失的块会以 I/O 错误失败。摘要和端点在一次运行期间固定；运行中途不会回退到 registry。缓存端点/令牌会从隐式继承的 guest 环境变量中移除。

服务端在应答 `prepare` 前仍会完整准备未缓存的镜像。这是客户端侧的懒加载，不是服务端的惰性 OCI 层解包。FUSE 适配器和现有 virtio-fs worker 目前同步处理请求：一次缓存未命中可能延迟无关的文件系统请求。不使用显式 vCPU 暂停。磁盘缓存配额/淘汰、原始 OCI xattr 和异步 virtio-fs 完成不在此实现中加入。

公共 Rust 客户端是 `pvisor::cache::CacheClient::from_env()`，它是阻塞式的。`cache prepare/list/stat/read` 使用显式后端配置，不使用 VM 默认 socket 探测的本地回退策略。

Unix socket 权限为 0600，并要求两端为同一有效用户。锁可防止两个服务端占用同一 socket；重启时会回收陈旧 socket，但普通文件、符号链接或活跃监听者绝不会被移除。请把 socket 放在由服务用户拥有的目录中。用 Ctrl-C 停止前台服务端可能留下陈旧 socket；无需手工清理。

对远程服务端，请在 SSH 隧道内使用带认证的 loopback TCP：

```sh
# 服务端：通过你的密钥管理/命令行设置一个强共享密钥。
export PVISOR_CACHE_TOKEN='YOUR_RANDOM_SECRET'
pvisor cache serve --listen tcp://127.0.0.1:7447

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

总量统计服务端已解包镜像中的常规文件路径及其未压缩逻辑大小，包括空文件和每个硬链接名。目录和符号链接不计。这些总量在仅元数据扫描后按 manifest 摘要缓存。它们描述整个镜像，不是压缩后的 OCI 层大小。不提供总量的旧服务端显示 `?`。

## 线格式

每个连接只承载一个请求和一个响应，然后关闭。一帧是四字节无符号大端 JSON 字节长度，后接 UTF-8 JSON。JSON 帧上限 1 MiB。成功的 `read` 响应帧之后紧跟恰好 `length` 个原始字节。其他响应没有二进制体。没有未经请求的消息或压缩。

请求信封：

```json
{"version":1,"token":null,"request":{"op":"read","digest":"sha256:...","path":[101,116,99,47,111,115,45,114,101,108,101,97,115,101],"offset":0,"length":1048576}}
```

路径和目录名是 Unix 文件名字节的 JSON 数组，以保留非 UTF-8 文件名。路径相对于镜像根；空路径表示根目录。绝对路径、向上遍历和 NUL 会被拒绝。符号链接作为元数据返回，服务端路径解析绝不跟随它们。guest 文件系统遍历在 guest 树内解析符号链接。

| `op` | 字段 | 响应 `status` |
| --- | --- | --- |
| `ping` | 无 | `ready`（协议 v1） |
| `prepare` | `image`、`architecture`（`amd64` 或 `arm64`）、可选 `refresh`（默认 false） | `prepared`：`digest`、`architecture`、`env`、`entrypoint`、`cmd`、可选 `totals`（`files`、`bytes`）、可选 `metadata_generation` |
| `list` | `digest`、`path`、`offset`（条目索引，从 0 开始） | `entries`：已排序的 `names`、可选对齐的 `metadata` 数组、`next_offset`（完成时为 null） |
| `stat` | `digest`、`path` | `metadata`：`kind`、`size`、`mode`、`uid`、`gid`、`inode`、`nlink`、`mtime`、`mtime_nsec`、`target` |
| `read` | `digest`、`path`、`offset`（字节偏移）、`length`（1..1048576） | `data`：`length`、`sha256`，后接原始字节 |

`prepare` 为客户端架构请求 Linux 镜像，与服务端架构无关。成功的已准备镜像记录持久化到
`<image-store>/metadata/prepared-v1/`，包含平台摘要和启动配置。可变 tag 复用记录五分钟；不可变摘要记录在其解包 root 存在期间不过期。`cache prepare IMAGE
--refresh`（协议 `refresh: true`）强制 registry 解析。刷新失败会返回错误并保留之前的记录；registry 请求有
10 秒连接超时和 300 秒总超时。过期的 tag 不会静默回退到陈旧数据。缺失/损坏的记录或缺失 root 会重新准备。按引用和架构的锁覆盖解析与准备，
因此并发请求会复查并复用首个成功结果。准备可能填充一个未缓存镜像，并保留现有的按摘要解包锁。`read`、`stat` 和
`list` 要求已准备的摘要；它们绝不隐式拉取镜像。

目录页包含与 `stat` 相同的属性，避免为每个子项单独请求。页最多 256 个条目，并会缩小以适应 JSON
帧上限（含长字节数组名和符号链接目标）。旧服务端的仅名字响应仍通过逐个 `stat`
请求兼容。持久化的页在多次挂载间保留其属性。

`kind` 为 `file`、`directory`、`symlink` 或 `special`。`mode` 包含 Unix 类型与权限位。`target` 包含符号链接字节或 null。属性反映服务端解包后的文件系统；v1 不重建原始 tar 所有权、不提供 xattr，也不定义跨服务端可移植的 inode ID。只有常规文件可读。短读（包括零字节）表示 EOF。客户端必须在校验通过后才把字节放入缓存，检查内容体长度和 SHA-256。所提供的哈希用于检测传输损坏；它不是对不可信服务端的独立证明。服务端及其本地镜像存储是受信任的。

错误是如下帧：

```json
{"status":"error","code":"not_found","message":"..."}
```

错误码有 `not_found`、`permission_denied` 和 `request_failed`（含无效参数、不支持的协议版本和认证失败）。分帧错误也可能导致断开。客户端必须把提前关闭、截断内容体或坏校验和视为失败，绝不当成缺失文件或零填充内容。错误消息用于说明，不保证机器稳定。

服务端为最多 4096 个 stat 响应和 128 个已排序目录索引共享内存缓存。目录分页复用同一索引，而不是每页重新扫描和排序。达到容量时，LRU 淘汰移除一个条目而不是清空缓存。文件系统 I/O 在缓存锁之外运行；并发未命中可能重复一次读取而不阻塞无关命中。这些内存缓存在服务端重启后惰性重建。

服务端有 16 个请求/文件 worker，最多 16 个排队连接。多余连接会被关闭；客户端可以重试。带认证的 prepare 请求转入单独的 2 worker 池，队列 16 个请求；满时服务端返回显式 busy 错误。registry 等待与解包不占用文件 worker。入站请求读取有五秒不活动超时；响应读写保留 300 秒超时，TCP 连接有 10 秒超时。长时间准备可能比断开的客户端活得更久；重试是安全的。关闭时不会优雅取消单个 OCI 下载。registry 下载限制和缓存淘汰沿用现有镜像存储；v1 不增加配额或淘汰。

不存在 vCPU 暂停/恢复消息。下载发生在宿主侧 FUSE 服务端，在沙箱化的 VM runner 进程之外。

## 验证后端

`just test pvisor` 包含服务器兼容性、文件系统/S3 独立 CLI 读写、只读权限、损坏拒绝与配置优先级检查。S3 本地夹具独立验算 SigV4（包括临时会话令牌），不使用真实账号、公网请求或外部 daemon。

有 KVM/FUSE 与静态 musl guest target 的 Linux x86_64 主机可以执行真实 VM 验收：

```sh
cargo nextest run --locked -p pvisor --test cache_backends --run-ignored ignored-only
```

此验收从两个后端启动 VM，删除发布端暂存目录后校验 guest 文件内容，并验证继承宿主环境时不会泄漏存储凭据。
