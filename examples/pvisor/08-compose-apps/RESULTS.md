# 实机部署记录

环境：Linux x86_64，真实 KVM，当前工作树的 `target/debug/pvisor`。
本记录来自 2026-11-14 本机运行，不是 Docker Engine 的运行结果。
所有 HTTP 端口只绑定 loopback。运行后均尝试 `down -v`，清理结果单独记录。
下面分别保留修复后复验及最初未修复运行时的失败、对照记录。

## 新增三组：CouchDB、RabbitMQ、Prometheus/Grafana 业务通过

这是三个独立场景的最终结果，不是声称首次批次全部成功。
RabbitMQ/监控报告为 `.work/20261114-174039-f9531a/report.json`；
CouchDB 报告为 `.work/20261114-175146-cached-203b96/report.json`。

| 场景 | 实际业务与重建断言 | 总耗时 | 最终清理 |
| --- | --- | --- | --- |
| CouchDB 3.4.3 + Nginx | 文档写入、64 KiB 二进制附件逐字节读取、Mango 查询、旧 revision 更新返回 409、匿名 401、数据库/代理重建后同一 revision 和附件保留、删除后 404；数据库进程四个 UID 均为 5984 | 31.661 秒，不含已有镜像的拉取 | pass，容器记录与卷目录均为空 |
| RabbitMQ 4.1.8 management + Nginx | durable queue、delivery_mode=2 二进制消息、管理 HTTP API 发布/消费/重新入队、重建后逐字节读取、确认消费后队列为空、删除队列、匿名 401 | 46.712 秒 | pass，容器记录与卷目录均为空 |
| Prometheus 3.5.0 + Grafana 11.6.0 | init 成功依赖、非 root TSDB、真实 self-scrape、Grafana datasource proxy 经 DNS 查询、匿名数据源访问被拒；全栈重建后 dashboard/数据源及旧时间点的 WAL 样本保留，同时查询到新的 process-start 值 | 77.613 秒 | pass，容器记录与卷目录均为空 |

RabbitMQ 只验证管理 HTTP API 驱动的真实队列，不声称 AMQP 客户端协议已验证。
监控历史查询时间为 `1794649338.606`，旧 process-start 为 `1794649311.07`，
重建后实时值为 `1794649346.06`。旧时间点仍读到旧值，并且实时值确实变化，
不是创建同名新指标后就宣称历史数据持久化。
CouchDB 重建后确认的 revision 为 `2-54329b99bcabed12b584dcfa5e6b7e5f`。
两项 64 KiB 二进制数据的 SHA-256 均为
`7daca2095d0438260fa849183dfc67faa459fdf4936e1bc91eec6b281b27e4c2`。

本轮新镜像 manifest：

- CouchDB：`sha256:67c4a24dd47d9a9728eb7205cdc44619d540da648f74c6ebbbf03a9c57533f2e`。
- RabbitMQ：`sha256:974220adc602b36b9a8e6be1b9535fd46315ea6075c5a5f5784a231bdd4fa563`。
- Prometheus：`sha256:8672a850efe2f9874702406c8318704edb363587f8c2ca88586b4c8fdb5cea24`。
- Nginx/Grafana/Alpine 的完整 manifest 同样保存在相应报告中。

通常在线执行：

```bash
python3 examples/pvisor/08-compose-apps/deploy.py --binary target/debug/pvisor \
  --case couchdb --case rabbitmq --case monitoring
```

**本轮 CouchDB 使用本地缓存路径**：Docker Hub 鉴权端连续 TLS 握手失败后，
显式将首轮 CouchDB/Nginx 和监控场景 Alpine 的完整本地 OCI catalog 条目
复制到新私有 store，再执行真实 `compose config/up/exec/down` 及同一个
`service_apps.couchdb` 业务函数。没有 mock 服务，没有跳过业务、身份或清理断言。
报告的 `catalog_sources`、`image_preparation_mode` 和 manifest 明确记录这一点；
不能把此通过写成“当前远端 pull 已恢复”。首次失败及各配置调整如下保留。

本轮只改应用配置、runner 和文档，没有修改 pVisor 运行时。
新增 init 测试只允许显式声明的成功 job 退出，失败/缺失/仍运行的 init，
以及意外退出的常驻服务都会失败。
`mise run test -- pvisor-cli -- -E 'binary(compose_cli) | binary(docker_cli) | test(cli::docker)'`
为 29/29；Python runner 单测为 10/10，源码编译及 diff 空白检查通过。
没有重跑全 workspace、此前九个 ignored KVM case 或 macOS/Windows 验收。
本次不覆盖 CouchDB/RabbitMQ 集群、AMQP 客户端、TLS 或监控高可用。

## 文档、消息和指标场景：首次尝试及配置诊断

新增 CouchDB/Nginx、RabbitMQ/Nginx、Prometheus/Grafana 三组实际服务，
不是 HTTP 假后端。首次运行：
`.work/20261114-172807-d64995/report.json`。

| 场景 | 首次结果 | 证据 | 清理 |
| --- | --- | --- | --- |
| CouchDB + Nginx | 失败，未计为业务通过 | `/_up` 就绪一直 502；入口没有成功启动数据库 | pass，无残留卷 |
| RabbitMQ + Nginx | 通过 | durable queue、delivery_mode=2 的 64 KiB 二进制消息、消费并重新入队、重建后逐字节读取、确认消费后队列为空、删除队列、匿名 401 | pass，无残留卷 |
| Prometheus + Grafana | 失败，未计为业务通过 | Prometheus 明确报 `queries.active: permission denied`，Grafana proxy 返回 502 | pass，无残留卷 |

RabbitMQ 首次耗时 80.784 秒；通过管理 HTTP API 操作真实 broker，
没有把它表述为 AMQP 客户端协议验收。所有服务的数据目录均声明命名卷，
只发布 loopback HTTP 端口。

### 两项应用配置问题，未修改运行时

- CouchDB 的官方 root 入口对 `/opt/couchdb` 运行 `find ... -exec chown -f ...`。
  只读挂载的 INI 文件不允许这种变更；独立入口 trace 在该命令后退出，
  静默的 `-f` 使原日志只剩 Tini 警告。trace 目录：
  `.work/entrypoint-diagnostic-1794649239797782600/`。
  直接改成 `5984:5984` 也没有通过：入口仍要为 `NODENAME` 修改 `vm.args`，
  日志明确报权限不足，见 `.work/20261114-174619-cached-68fad5/`。
  配置进一步改为：INI 只读挂在 `/run`，启动前复制到 guest 私有 rootfs，
  再 exec 官方 Tini/入口脚本执行权限准备和降权；断言实际 Erlang daemon
  的四个 UID 都是 5984。init 服务授权持久化 database 子目录。
  不关闭认证，不放开宿主只读绑定，不忽略失败的 chown。
- 初版 Prometheus init 只 chown 了空卷根目录。后续镜像挂载时，空卷依然
  触发已有的 image volume copy-up，覆盖准备过的根属性。独立检查确认：
  init 后宿主虚拟元数据为 `65534:65534:040755`，后续 guest 的 `/prometheus`
  为 `1000:1000 775`；官方进程仍为 `65534:65534`，touch 明确被拒。
  改为 init 创建并授权 `/prometheus/data`，让数据卷具有明确的非空内容，
  Prometheus TSDB 使用这个子目录。没有把 Prometheus 改成 root 或 chmod 777。

复验使用同样的业务断言、就绪预算和完整卷清理条件；结果另行记录，不覆盖首次失败。

`.work/20261114-174039-f9531a/` 中 RabbitMQ 和修正卷初始化的
Prometheus/Grafana 分别通过（46.712 秒、77.613 秒），清理均为空。
CouchDB 在该次以及 `.work/20261114-174313-e9eaf3/` 的显式重试中，
均被 Docker Hub `auth.docker.io` 的 `tls handshake eof` 阻断；
这些不是数据库启动或业务通过。
后续 CouchDB 业务测试明确复用首轮已完整拉取的 CouchDB/Nginx，
以及监控场景已完整拉取的 Alpine 镜像目录；不重新声明远端拉取已成功。

## 最终验收：两栈业务、重建持久化和完整卷清理通过

最终报告：`.work/20261114-171628-ede4e0/report.json`。
使用当前构建的 CLI、管理卷 symlink ownership 修复、Redis 显式命名卷，
以及同时检查容器记录和卷目录的加强版 cleanup 判据：

```bash
python3 examples/pvisor/08-compose-apps/deploy.py --binary target/debug/pvisor \
  --case nextcloud --case minio --nextcloud-install-timeout 600 \
  --minio-build-manifest target/compose-acceptance/minio-source/build.json
```

| 应用栈 | 业务与持久化 | 总耗时 | 最终清理 |
| --- | --- | --- | --- |
| Nextcloud + PostgreSQL + Redis | Nextcloud DB/Redis adapter 实际读写；WebDAV 64 KiB PUT/GET/PROPFIND、匿名 401、全栈重建保留、DELETE 后 404 | 309.239 秒 | pass，容器记录和卷目录均为空 |
| MinIO + Nginx | 代理和 DNS；SigV4 bucket 创建、1 MiB PUT/GET/List、匿名拒绝、全栈重建保留、DELETE 后 404 | 33.525 秒 | pass，容器记录和卷目录均为空 |

Nextcloud 首次安装就绪为 **257.919 秒**，预算明确为 600 秒，默认的 150 秒失败
没有改写。文件 SHA-256 为
`7daca2095d0438260fa849183dfc67faa459fdf4936e1bc91eec6b281b27e4c2`，
配置 SHA-256 为 `73bfb30396a610530bc4e2308e91f432099d92bc94c68f82c4534475f7af9627`。

MinIO 为下文固定官方 release 的真实源码本地构建，不是 Quay 镜像访问已恢复。
最终本地镜像 digest：
`sha256:5d4207d22b16b99aa6c33719eef136209e5966d9971814b559757003250e736b`。
源码和二进制摘要与下文一致；重新打包的镜像 digest 单独记录，不假设打包可复现。
1 MiB 对象 SHA-256 为
`fbbab289f7f94b25736c58be46a994c441fd02552cc6022352e3d86d2fab7c83`。

报告中两项均为 `outcome=pass`、`cleanup=pass`、
`remaining_volume_directories=[]`；runner 退出后再次独立检查了这两个私有 store，
容器记录数和卷目录项数都为零。历史匿名卷遗漏记录如下保留。
不把这次本地验收扩展为完整上游 Compose、生产配置或跨平台支持声明。

## 历史首次同次业务通过：旧清理判据仍有遗漏

运行目录：`.work/20261114-165921-6cd7ff/`。使用默认数据库 socket 路径，
没有去掉 rsync ownership 参数、忽略 chown 失败或用 SeaweedFS 替代 MinIO。

```bash
python3 examples/pvisor/08-compose-apps/deploy.py --binary target/debug/pvisor \
  --case nextcloud --case minio --nextcloud-install-timeout 600 \
  --minio-build-manifest target/compose-acceptance/minio-source/build.json
```

| 应用栈 | 完成的业务断言 | 总耗时 | 清理 |
| --- | --- | --- | --- |
| Nextcloud 31.0.10 + PostgreSQL 16 + Redis 7 | Nextcloud 自身 DB adapter 查询 PostgreSQL；Redis adapter 缓存写/读/删；WebDAV 64 KiB 二进制 PUT/GET/PROPFIND；匿名 401；全栈重建后原文件和数据库元数据保留；DELETE 后 404 | 338.571 秒 | pass |
| 本地构建的上游 MinIO + Nginx 1.28 | 代理 readiness、项目 DNS、SigV4 bucket/object CRUD、1 MiB 二进制及 ListObjectsV2、匿名拒绝、全栈重建后保留、删除后 404 | 34.030 秒 | pass |

Nextcloud 文件 SHA-256：
`7daca2095d0438260fa849183dfc67faa459fdf4936e1bc91eec6b281b27e4c2`。
实际镜像 manifest：

- Nextcloud：`sha256:615e6709a1105b0e0d63396565c123e2de78677153694d43172721923b128bc9`。
- PostgreSQL：`sha256:a2f7e20374cd8fd8ed4a588b79549e10589bea7b1686e77922f5ac741b80b8a1`。
- Redis：`sha256:e4ee3e03ce8f75471f40937a7c6b2b0e500390b6812fcdf5cb32c01295ded446`。
- 本次 MinIO 本地镜像：`sha256:cc0cdf669ed53eb6324ad43a0bf54405c762098064ced9f96d9ff73d3e825dcd`；
  源码和二进制摘要与下一节相同。

Nextcloud 首次安装就绪为 282.786 秒，显式记录 600 秒预算，而不是把默认
150 秒运行算作通过。复制进度的文件数量在约 60/123/191/255 秒分别为
5,733 / 13,712 / 22,172 / 26,453，前三次进程快照仍有 rsync，未出现原来的
symlink chown 错误。`.work/20261114-165439-d89d6e/` 的 150 秒失败仍保留，
它只有初始化日志，没有进入 WebDAV 验收；不能将它与新成功报告混为一次。
以上耗时仅解释就绪预算，不是性能基准。

管理卷修复将 symlink guest UID/GID 存入隐藏的卷内私有 catalog，
不向宿主执行 guest UID chown，不跟随 link target。新增测试验证非 root 创建、
setgid、重命名/硬链接/重开、并发 UID/GID 更新、保留 pin 的已删除 inode、
元数据隐藏、防篡改和损坏拒绝。普通 bind share 权限不变。
该能力仍限 Linux，要求后端 inode birth time 和目录 xattr；
symlink chown 的 ctime 和历史记录 GC 不是本次已完成的能力。

两栈 `down -v` 后额外检查容器记录数均为零。没有发布镜像或改变共享分支。
这里的历史 `cleanup=pass` 使用旧判据：`down -v` 返回成功且没有容器记录。
后续 `.work/20261114-170820-77ab6b/` 再次业务通过（Nextcloud 安装 278.597 秒、
总计 333.736 秒；MinIO 33.097 秒），但额外的卷目录审计发现 Redis `/data`
在第一次 `down/up` 后遗留了一个旧匿名 `dump.rdb` 卷。最终 `down -v` 只能移除
当时仍有关联的匿名卷，不能清掉之前已脱离容器的匿名卷；这些历史报告不能当作
“所有测试数据已清空”的证据。旧数据留在原私有运行目录，没有盲目删除。
配置现为 Redis `/data` 显式声明 `redis-data` 命名卷，并将“卷目录为空”纳入
runner 的 cleanup 判据；遗漏数据会使整个 case 失败，即使 down 返回 0。

### 本轮回归与待跟进项

使用 `mise run test` 的针对性回归：

- `pvisor-vm` 的 virtio-fs/API/边界测试：297/297。
- `pvisor` Docker 测试：95/95。
- `pvisor-cli` Compose/Docker 普通测试：29/29。
- 原有九个 Compose KVM case 串行执行：9/9。
- Python runner 本地契约：8/8（HTTP、构建来源/摘要和卷清理判据）。
- Clippy 全目标 `-D warnings`、rustdoc `-D warnings`、
  daemon/shim 编译、格式及 diff 空白检查通过。

另有一次与 Nextcloud 初始化同时运行的四并行 KVM 回归为 **8/9**：
`compose_native_health_dependencies_and_file_binds` 的依赖检查读到 migration
退出码 137，失败收集时同一服务记录已是退出码 0，并输出 migration-completed。
随后独立串行运行九个 case 全部通过。不能由此推断 OOM、外部负载或具体竞态根因，
也不能把第一次并行失败改写成通过；需要后续针对代次/完成记录观察单独复现。
本次没有为了取得绿色结果修改测试期望或忽略该 case。
这不影响上面两套真实应用完整业务断言的独立通过，但并发稳定性仍有待调查。

没有运行全 workspace 测试、macOS/HVF 或 Windows/WHP 验收。

## MinIO + Nginx：官方源码本地构建验收通过

运行目录：`.work/20261114-164758-707cd1/`。执行：

```bash
python3 examples/pvisor/08-compose-apps/build_minio.py
python3 examples/pvisor/08-compose-apps/deploy.py --binary target/debug/pvisor \
  --case minio --minio-build-manifest target/compose-acceptance/minio-source/build.json
```

官方 Quay 镜像仍返回 401，官方历史二进制下载站返回 410。本次没有把这些失败
记作成功，而是从官方 `RELEASE.2025-09-07T16-13-09Z` 源码构建真正的 MinIO，
以本地 `pvisor-local-minio:source` 镜像部署。版本差异和本地构建来源都明确记录。

- 源码归档 SHA-256：`c9598dcce3440977e79f787f2ba0e7e4d92c8d556bd51e7cef3785bafd6635f3`。
- 二进制 SHA-256：`85e810a66e6bf5af2f1939acb240791a23b39a47f8d5d7c37c57b1a5064becb7`。
- Go：`go1.27.3-X:nodwarf5 linux/amd64`，`CGO_ENABLED=0`，不伪造 release 版本字符串。
- 本地镜像 digest：`sha256:7ae1ebae5e296159d4a888905969dcf204c02b65c8c9b4b94a46b61f1edf083f`。
- Nginx 代理 readiness、项目 DNS、SigV4 创建 bucket、1 MiB 二进制 PUT/GET、
  ListObjectsV2、匿名访问拒绝全部通过。
- `down/up` 确认容器 ID 全部替换，原 bucket 和对象逐字节保留；
  签名 DELETE 后 GET 返回 404。
- 对象 SHA-256：`fbbab289f7f94b25736c58be46a994c441fd02552cc6022352e3d86d2fab7c83`。
- 业务/重建/清理总耗时 33.304 秒，`cleanup=pass`。不包含首次 Go 依赖下载与编译。

这是单实例 MinIO + Nginx 的业务验收，不覆盖分布式、TLS、multipart、STS 或 SSE。
下面的首次镜像拉取失败及独立 SeaweedFS 结果保持不变。

## 扩展验收：新增 2 个业务通过、1 个运行时阻塞、1 个镜像访问阻塞

本阶段只增加测试配置、业务断言和 runner，没有修改运行时。
结果来自下面列出的独立运行，不是把失败 case 从同一报告删掉后声称全绿。

| 应用栈 | 最新结果 | 业务/失败证据 | 运行目录 |
| --- | --- | --- | --- |
| WordPress + MariaDB | 回归通过 | 安装、文章发布/读取及重建保留 | `.work/20261114-141010-2b38ee/` |
| Gitea + PostgreSQL | 回归通过 | 注册、仓库和初始 commit、重建保留 | 同上 |
| Nginx + Grafana | 回归通过 | 代理、认证 dashboard 写入及重建保留 | 同上 |
| Celery + Redis + Flower | 通过 | 认证发现 worker；真实 prefork worker 计算 20+22=42；Redis AOF 在全栈重建后仍返回原任务结果 | `.work/20261114-142256-b60be3/` |
| SeaweedFS + Nginx | 通过 | SigV4 认证、bucket 创建、1 MiB 二进制 PUT/GET/List、匿名 403、重建保留、删除后 404 | `.work/20261114-142601-484bb5/` |
| Nextcloud + PostgreSQL + Redis | 失败 | 初始化 rsync 的 symlink chown 返回 EOPNOTSUPP；未进入 WebDAV 业务验收 | `.work/20261114-140848-39d7d3/` |
| MinIO + Nginx | 未部署：拉取失败 | 官方 Quay 镜像返回 401；匿名 token 仍不能取得 manifest，不能评价应用运行结果 | 同上 |

所有运行的 `cleanup` 均为 `pass`，另行检查了容器记录已清空。
原有三栈本轮回归耗时分别为 53.007、70.269、74.725 秒；
Celery 最终部署/业务/重建/清理共 31.524 秒，SeaweedFS 共 87.239 秒。
这些包含拉取/准备，并不是性能基准或 SLA。

### 保留的首次失败及其分类

`.work/20261114-140848-39d7d3/` 包含 Nextcloud、MinIO、Celery 首次尝试。
三个当时均失败，记录未覆盖：

- Nextcloud 安装就绪超时 150 秒，但应用日志已出现大量
  `rsync: [generator] chown "...js.map.license" failed: Operation not supported (95)`。
  相应源文件经 `stat` 确认为符号链接。独立的最小 rsync 复制复现也返回
  **rsync 退出码 23**，目标普通文件为 `33:33`、符号链接仍为 `0:0`。
  不提高超时、不去掉 ownership 参数来掩盖此限制。
- MinIO 原参考 `quay.io/minio/minio:RELEASE.2025-09-06T17-38-46Z`
  在 manifest 阶段被拒。独立匿名 bearer-token 流程也得到 401；
  Docker Hub 官方 `minio/minio` 的相应 release 请求同样返回 401。
  这是本环境中的镜像访问阻塞，不作为 pVisor 拉取实现错误的充分证据。
  SeaweedFS 是另一项目的独立 case，不是 MinIO 替代镜像或 MinIO 通过声明。
- Celery worker 5.3.1 日志已经显示连接 Redis 并 ready，首次 Flower inspect
  则早于 worker 就绪。Flower 2.0.1 的 `/api/workers` 默认返回缓存，
  runner 初次没有请求刷新，因此误判。改用上游支持的 `refresh=1` 后完整业务通过，
  不登记为运行时缺陷，仍保留原始超时记录。

最小 Nextcloud ownership 复现的 guest 命令（在私有诊断 store 中使用同一镜像、
命名卷 `/data`，不连接网络）：

```sh
mkdir -p /tmp/src /data/copy
touch /tmp/src/regular
ln -s regular /tmp/src/link
rsync -rlD --chown=33:33 /tmp/src/ /data/copy/
# rsync: chown "/data/copy/link" failed: Operation not supported (95)
# rsync error: some files/attrs were not transferred ... (code 23)
```

### 新业务的精确证据

Celery 原任务 ID：`42ac47df-a3dc-4613-8671-44298479f13b`。
重建后通过 `AsyncResult` 查询同一 ID，状态为 SUCCESS，结果为 42；
不是重新提交任务得到同一个数值。

SeaweedFS 对象大小为 1,048,576 字节（覆盖全部 byte 值），
SHA-256 为 `fbbab289f7f94b25736c58be46a994c441fd02552cc6022352e3d86d2fab7c83`。
GET 返回逐字节相等，重建后再次读取同一对象，然后签名 DELETE 并确认 GET 404。
未测试 multipart、STS、SSE 或分布式故障恢复。

Runner 本地 HTTP 契约单测 4/4 通过，覆盖二进制、HTTP 方法、原有 JSON/form
行为、cookie 隔离及错误脱敏；不以这四项单测替代上面的真实 VM 业务验证。

## 修复后：默认 socket 路径 3/3 通过

运行目录：`.work/20261114-134559-2004ad/`。
使用新构建的 CLI，执行：

```bash
python3 examples/pvisor/08-compose-apps/deploy.py --binary target/debug/pvisor
```

未使用 `--database-sockets-in-shm`，三个报告的 `database_socket_variant` 均为
`default`。没有修改数据库初始化 SQL、放开 host setuid 或增加 guest capability。
本次修复是 Linux 私有 rootfs 的普通/socket MKNOD 身份投影，以及非
`localhost` 的合法每启动 hostname；自动生成的 hosts 模板随冷启动刷新。

| 应用栈 | 业务及持久化结果 | 总耗时 | 最终清理 |
| --- | --- | --- | --- |
| WordPress + MariaDB | HTTP 安装、WordPress 发布文章及 HTTP 读取成功；新容器仍读到文章 | 64.002 秒 | 通过 |
| Gitea rootless + PostgreSQL | HTTP 注册、API 创建真实仓库及初始提交；新容器保留仓库描述和 commit SHA | 75.257 秒 | 通过 |
| Nginx + Grafana | 代理健康接口和认证 dashboard 创建成功；新容器仍读到 dashboard | 80.639 秒 | 通过 |

Gitea 本次创建仓库耗时 23.31 秒，仍使用已声明的 60 秒操作上限。
以上仍然只是 pVisor 子集改编配置的通过，不等于上游原文件的完整兼容。

## 上游原文件：配置校验不通过

| 来源 | 下载内容 SHA-256 | 首个错误 |
| --- | --- | --- |
| [Awesome Compose WordPress](https://raw.githubusercontent.com/docker/awesome-compose/master/wordpress-mysql/compose.yaml) | `1a7d44cefdb903ae9dbaa785342fbe99a7f08e5b31be99cb9145695defe3938d` | 不支持 `restart` |
| [Awesome Compose Flask/Redis](https://raw.githubusercontent.com/docker/awesome-compose/master/flask-redis/compose.yaml) | `e74ce3a1493a0be8f7483acabe0c6902b045db48d2d174b267f2bad0083aa288` | 不支持 `stop_signal` |

这里只执行 `config -q`，没有把原文件部署到宿主公开端口。
首个未知字段之后可能还有其他不兼容项，不能据此声称只补这一个字段就能运行。

## 历史：修复前明确改编后的默认部署

运行目录：`.work/20261114-121602-0c91f2/`，报告为其中的 `report.json`。

| 应用栈 | 实测结果 | 业务验证 | 清理 |
| --- | --- | --- | --- |
| WordPress + MariaDB | 失败 | MariaDB 在初始化 Unix socket 时退出，WordPress 未启动 | 通过 |
| Gitea rootless + PostgreSQL | 失败 | PostgreSQL 在初始化 Unix socket 时退出，Gitea 未启动 | 通过 |
| Nginx + Grafana | 通过 | 经代理认证创建 dashboard；down/up 换掉两个容器后，读取同一 dashboard 成功 | 通过 |

MariaDB 的实际错误：

```text
Can't start server : Bind on unix socket: Operation not permitted
socket: /run/mysqld/mysqld.sock
```

PostgreSQL 的实际错误：

```text
could not bind Unix address "/var/run/postgresql/.s.PGSQL.5432": Operation not permitted
FATAL: could not create any Unix-domain sockets
```

这是数据库运行时兼容性阻塞，不是 Compose 配置解析成功就可以忽略的错误。
没有禁用数据库认证、改成 sleep 服务或跳过业务断言。

## 显式 `/dev/shm` 对照：保留失败，不改写默认结果

运行目录：`.work/20261114-122611-04b0ba/`。
通过 `--database-sockets-in-shm` 把 Unix socket 放到 guest 已有 tmpfs。
两个数据库都越过了 socket bind 的 EPERM，但仍然失败：

- MariaDB 临时服务器成功启动，随后初始化账号的 SQL 返回
  `ERROR 1133 (28000): Can't find any matching row in the user table`。
- PostgreSQL 临时服务器成功启动，但官方入口脚本清空 `PGHOST` 后，
  `psql` 又回到默认 `/var/run/postgresql` 路径，找不到 socket。

MariaDB 的后续阻塞与默认 hostname 有明确冲突：
guest 当时默认 hostname 是 `localhost`；镜像入口脚本先按 `@@hostname`
删除 `root@<hostname>`，随后又为 `root@localhost` 设置密码，导致目标账号已被删除。
参考[MariaDB 11.4 入口脚本](https://github.com/MariaDB/mariadb-docker/blob/master/11.4/docker-entrypoint.sh)
的 `docker_setup_db`。本轮没有篡改数据库初始化 SQL 来绕过它。

## Gitea 的进一步对照

运行目录：`.work/20261114-122930-c22280/`。
在显式对照模式下，将 PostgreSQL 原本为空的默认 socket 目录替换为指向
`/dev/shm` 的 symlink，保持官方初始化脚本仍可访问默认路径。
不递归删除目录、不增加 guest capability，也不实现或假装支持 Compose `tmpfs`。

实测 PostgreSQL 完成初始化并接受 TCP 连接；Gitea 完成 ORM 初始化、
HTTP 用户注册和 Basic Auth 用户查询。随后创建带初始提交的仓库触发了
runner 的 10 秒 HTTP 超时，所以这次**没有计为业务通过**，清理通过。

后续运行目录：`.work/20261114-123354-830fa2/`。保持相同配置与镜像，
仅对仓库创建使用明确的 60 秒上限；实际创建耗时 **19.846 秒**，不是卡死：

- HTTP 注册用户，Basic Auth 查询到该用户。
- REST API 创建真实仓库，提交列表中存在初始 Git commit。
- `down/up` 删除并创建新的数据库和 Gitea 容器。
- 同一用户仍可认证，仓库描述与初始提交 SHA 均保留。
- 部署、业务、持久化与最终 `down -v` 清理均通过，总用时 73.452 秒。

因此，**Gitea/PostgreSQL 当时在显式 socket 对照配置下通过**，默认配置仍失败。
第一次 10 秒超时属于本轮 runner 的操作预算不足，不能据此登记运行时功能缺陷。

## 历史结论与当时的修复建议

| 场景 | 默认 socket 路径 | 显式 socket 对照 |
| --- | --- | --- |
| Nginx + Grafana | 业务及重建持久化通过 | 不需要 |
| Gitea + PostgreSQL | Unix socket EPERM，失败 | 业务及重建持久化通过；需要 socket 目录映射 |
| WordPress + MariaDB | Unix socket EPERM，失败 | 越过 socket 后仍因 hostname/account 初始化冲突失败 |

当时确定的两个运行时兼容性问题（已由顶部的默认路径复验覆盖）：

1. 普通数据库在 guest rootfs 上创建 Unix socket 返回 EPERM。需要独立验证
   socket 节点创建、guest 身份和根文件系统适配，不能简单放开全部节点或 capability。
2. 默认 hostname 固定为 `localhost` 与 MariaDB 入口脚本冲突。需要选择合法且
   不与 loopback 保留名冲突的实例 hostname，同时保持之前 HTTP 服务本机解析的修复。

原始 Compose 的 `restart`/`stop_signal` 是另外的已知配置能力缺口。
最初探索阶段没有修改运行时；后续修复了上述两个运行时问题，
没有在这一步扩展 `restart`/`stop_signal` 配置能力。

## 实际拉取的镜像

| 镜像 | Manifest digest |
| --- | --- |
| `mariadb:11.4`（11.4.13） | `sha256:f6eca93803e34766e787a96bad9e2ac5abbcdcb8e7f390a6f34e9bf3eee525e6` |
| `wordpress:6.8.3-php8.3-apache` | `sha256:362e268b6a8d69c1f2384cd0a4be4de9bf0f02d8349a1a37e1cd30352a26abf7` |
| `postgres:16-alpine`（16.16） | `sha256:a2f7e20374cd8fd8ed4a588b79549e10589bea7b1686e77922f5ac741b80b8a1` |
| `gitea/gitea:1.24.7-rootless` | `sha256:5c7658276141f1086f0ed2bed2c745c6a17189db5a4d6c18f93298db081e1680` |
| `grafana/grafana:11.6.0` | `sha256:b739cda4b61ba3b90707578b643a22cd851fecf4498e6c6ec2d8f9d622a5d0b2` |
| `nginx:1.28-alpine` | `sha256:0dcc88822d45581e65ae329f8be769762bf628d3b2bb7d2a077d4aa5c98b30e3` |
| `nextcloud:31.0.10-apache` | `sha256:615e6709a1105b0e0d63396565c123e2de78677153694d43172721923b128bc9` |
| `redis:7-alpine` | `sha256:e4ee3e03ce8f75471f40937a7c6b2b0e500390b6812fcdf5cb32c01295ded446` |
| `mher/flower:2.0.1`（Celery 5.3.1） | `sha256:611ce12597391b2fe351aabaf8632f92c2ac3d13156699254d4609c765516729` |
| `chrislusf/seaweedfs:3.80` | `sha256:7c981e18362bc70e88b1e9756dff7b8c2b61bfb1db295097b97244c0e6e85cfc` |

这些 tag 可能随时间变化，复测应同时比较报告中的 digest、配置 hash 和对照选项。
`.work` 可能含应用密钥，不应整体共享；本文件及脱敏报告不包含生成的密码。
