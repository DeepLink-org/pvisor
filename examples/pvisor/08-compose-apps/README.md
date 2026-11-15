# 真实应用的 Compose 部署验收

这里部署公开发布的应用镜像，不用 sleep 服务或模拟后端替代应用。
这是独立、显式运行的本地验收，不加入默认 `mise run examples`。
历史失败、对照实验及修复后默认路径的验收见 [RESULTS.md](RESULTS.md)。
WordPress、Gitea、Grafana、Celery/Flower、SeaweedFS 已通过业务与重建持久化验收。
Nextcloud 已在修复管理卷 symlink ownership 后通过业务及重建验收；
首次安装使用显式 600 秒预算，原 150 秒超时记录保留。
MinIO 已通过固定官方源码的本地构建路径；原 Quay 镜像 401 仍单独保留。
CouchDB/Nginx、RabbitMQ/Nginx 和 Prometheus/Grafana 也已通过业务与重建验收；
CouchDB 本轮复用先前已拉取的镜像，远端 TLS 拉取失败单独保留，不算作恢复。

## 场景与来源

| 场景 | 上游参考 | 业务验收 |
| --- | --- | --- |
| WordPress + MariaDB | [Awesome Compose WordPress](https://github.com/docker/awesome-compose/tree/master/wordpress-mysql) | HTTP 安装站点、通过 WordPress API 发布文章、HTTP 读取、down/up 后再次读取 |
| Gitea rootless + PostgreSQL | [Gitea 1.24 rootless 安装](https://docs.gitea.com/1.24/installation/install-with-docker-rootless) | HTTP 注册用户、REST API 创建带初始提交的仓库、down/up 后读取仓库 |
| Nginx + Grafana OSS | [Grafana Docker 安装](https://grafana.com/docs/grafana/latest/setup-grafana/installation/docker/) | 通过 Nginx 访问健康接口、认证创建 dashboard、down/up 后读取 dashboard |
| Nextcloud + PostgreSQL + Redis | [官方容器示例](https://github.com/nextcloud/docker/blob/master/.examples/docker-compose/insecure/postgres/apache/compose.yaml) | WebDAV 二进制 PUT/GET/PROPFIND、独立未认证访问拒绝、重建后读取及删除 |
| MinIO + Nginx | [官方 Compose 示例](https://github.com/minio/minio/blob/master/docs/orchestration/docker-compose/docker-compose.yaml) | SigV4 创建 bucket、1 MiB 二进制对象读写/列表、认证拒绝、重建后读取及删除 |
| Celery + Redis + Flower | [Flower 镜像](https://github.com/mher/flower/blob/v2.0.1/Dockerfile) | 认证发现真实 worker、prefork 执行后台任务、Redis AOF 结果跨重建保留 |
| SeaweedFS + Nginx | [SeaweedFS 3.80](https://github.com/seaweedfs/seaweedfs/tree/3.80) | 独立的 S3 case，使用同一 SigV4/二进制对象断言；不计作 MinIO 通过 |
| CouchDB + Nginx | [CouchDB 3.4.3 官方镜像](https://github.com/apache/couchdb-docker/tree/main/3.4.3) | 文档/64 KiB 附件、Mango 查询、旧 revision 冲突拒绝、重建保留及删除 |
| RabbitMQ + Nginx | [RabbitMQ 官方镜像](https://github.com/docker-library/docs/tree/master/rabbitmq) | 真实 durable queue/persistent message，通过管理 HTTP API 发布/消费/重新入队、重建保留及确认删除；不是 AMQP 客户端协议验收 |
| Prometheus + Grafana | [Prometheus 3.5.0](https://github.com/prometheus/prometheus/tree/v3.5.0) | init 服务初始化非 root 卷、真实 self-scrape、Grafana 数据源代理查询、历史样本及 dashboard 跨重建保留 |

这些是针对 pVisor 当前 Compose 子集的**明确改编配置**，不是上游文件原样兼容的声明：

- 不使用尚未实现的 `restart` 策略、自定义网络、`expose` 或 Docker secrets。
  默认项目网络仍连接各服务；这不等价于实现自动重启或网络分区。
- 发布的 HTTP 端口只绑定 `127.0.0.1`，由 runner 选择空闲端口。
- 使用固定应用版本/数据库版本族，实际拉取的 manifest digest 写入报告。
  WordPress 示例的旧 MariaDB 10.6 改为 11.4；Gitea 示例的 PostgreSQL 14 改为 16。
- 绑定数据目录改为 pVisor 管理的命名卷；增加数据库健康条件与显式 VM 预算。
- Gitea 关闭 SSH，只测 HTTP/API；使用安装锁定配置自动初始化数据库。
  Grafana 关闭遥测和更新检查，另外增加 Nginx 反向代理。
- Nextcloud 固定为 31.0.10 Apache/PostgreSQL 16，使用 Redis 密码及命名卷，
  Redis `/data` 也显式声明命名卷，避免 `down/up` 留下旧匿名 RDB 卷；
  去掉上游 cron、restart 和 SELinux `:z/:Z`，不宣称 cron/TLS/升级已验证。
- MinIO 默认尝试官方示例所用的 2025-09-06 镜像，显式源码构建模式固定为
  官方 `RELEASE.2025-09-07T16-13-09Z`；改为单实例命名卷和 Nginx；
  不测试四节点分布式、故障转移或 erasure coding。只绑定 S3 端口，不发布 console。
- Celery/Flower 共用 `mher/flower:2.0.1` 的实际 Celery/Redis 依赖；
  只读挂载 `tasks.py` 中的加法任务，不用同步本地函数替代队列执行。
  Redis 配置密码和 AOF，Flower 配置 Basic Auth；不宣称 Flower 历史本身持久化。
- Flower API 显式请求 `refresh=1` 重新 inspect worker，避免把首次 inspect
  早于 worker 就绪造成的空缓存误判为队列故障。
- SeaweedFS 使用单实例 `weed server -s3`，内部 master/volume/filer/gateway
  不是跨 VM 集群，不验证副本和故障转移。仅 Nginx 的 S3 HTTP 端口发布到 loopback。
  随机 S3 凭证 JSON 仅生成在私有 `.work`，只读挂载，成功清理后删除。
- CouchDB 使用单节点配置和数据库命名卷；Nginx 代理 HTTP，强制有效用户。
  init 服务创建并授权持久化 database 子目录；只读 INI 挂在 `/run`，
  启动前复制到 guest 私有 rootfs，再 exec 官方 Tini/入口脚本准备权限并降权。
  不忽略 chown 失败，不放开宿主绑定；实际检查 Erlang 数据库进程的 real/effective/
  saved/fs UID 均为 5984，重建后也检查。不能直接以非 root 跳过官方入口的准备，
  因为 `NODENAME` 仍需要修改镜像内的 `vm.args`。
  测试 revision 冲突，不覆盖复制、分片或集群故障恢复。
- RabbitMQ 使用管理插件镜像、稳定的 `RABBITMQ_NODENAME=rabbit@localhost`，
  避免每次 VM 启动的 hostname 变化切换 Mnesia 数据目录。
  单节点内存高水位显式设为 256 MiB、disk free limit 为 64 MiB，仅用于本地验收。
  Nginx 保留编码的 `%2F` vhost；不发布 AMQP 或 Erlang distribution 端口。
- Prometheus 使用官方非 root 用户，一次性 `init` 服务创建并授权卷内 data 子目录，
  通过 `service_completed_successfully` 依赖启动；不是把 init 退出当作服务故障。
  不只对空卷执行 chown：空卷在后续镜像首次挂载时仍会被 image volume copy-up 初始化；
  有 data/database 子目录后，不会把 init 的权限准备重新覆盖。
  每秒采集自身指标，Grafana 通过项目 DNS 访问 Prometheus，只有 Grafana 发布 loopback 端口。
  历史查询使用重建前保存的具体时间与 process-start 值，并要求重建后产生更新的值，
  不能用同名新样本冒充旧 WAL 数据。
- 密码每次随机生成，仅通过进程环境及应用的私有运行存储传递，不写入源配置和公开报告。

Runner 另行下载 Awesome Compose 的 WordPress、Flask/Redis，
以及 Nextcloud、MinIO 的上游原文件，
执行 **config 校验**，记录来源 URL、下载内容 SHA-256 和原始错误。
不会自动删除未知字段来使原配置通过，也不会把这个校验当作部署成功。

## 运行

需要 Linux x86_64/KVM、已构建的 pVisor CLI/固件、Python 3.10+、
Docker Hub 网络访问和数 GB 可用磁盘。镜像版本用于隔离测试，不构成生产安全或升级建议。

```bash
python3 examples/pvisor/08-compose-apps/deploy.py \
  --binary target/debug/pvisor
```

CLI 必须能找到匹配的固件（例如 `target/debug/libkrunfw.so.5`）。
可传 `--case wordpress`、`--case gitea`、`--case grafana`、`--case nextcloud`、
`--case minio`、`--case celery`、`--case seaweedfs`、`--case couchdb`、
`--case rabbitmq`、`--case monitoring`；可重复指定，省略则运行全部十个。
默认共用 `target/compose-acceptance/images` 镜像缓存；已有
`PVISOR_IMAGE_STORE` 环境变量优先。

例如只运行新增场景：

```bash
python3 examples/pvisor/08-compose-apps/deploy.py --binary target/debug/pvisor \
  --case couchdb --case rabbitmq --case monitoring
```

若 MinIO 官方镜像访问被 registry 拒绝，默认路径仍记录为拉取失败，不尝试
未知第三方镜像。当前可显式构建固定的官方源码（需 Python 3.12+、Go 1.24+、
GitHub 及 Go module 网络访问，首次编译会下载依赖）：

```bash
python3 examples/pvisor/08-compose-apps/build_minio.py
python3 examples/pvisor/08-compose-apps/deploy.py --binary target/debug/pvisor \
  --case minio --minio-build-manifest target/compose-acceptance/minio-source/build.json
```

构建脚本核对固定源码归档 SHA-256，每次从新解压的源码编译 Linux amd64 静态二进制，
保留 Go module 缓存，并记录源码、go.mod、编译器和产物摘要。
Runner 再核对二进制摘要，通过 pVisor build 打包 Alpine 基础镜像、真实 MinIO
二进制及 AGPL 许可证，只拉取 Nginx，不对本地 MinIO tag 执行 registry pull。
本地镜像和源码构建来源明确写入报告；不发布镜像，不宣称为官方预构建镜像。
`--case seaweedfs` 是另一实现的独立 S3 验收，不能代替 MinIO 的结果。

新镜像可能需要数 GB 下载与解压空间。每个栈顺序部署并清理，不同时保留全部栈。
Nextcloud 安装和 Flower worker 发现默认各有 150 秒就绪预算，Celery 结果等待
45 秒，S3 单操作 30 秒；这些是本地验收上限，不是性能承诺。
可显式传 `--nextcloud-install-timeout 600` 测试较慢的首次初始化：
报告记录所选预算及实际安装就绪耗时，初始化期间每分钟采集进程状态和文件数量。
这个选项不改变 rsync/ownership 或业务断言；原 150 秒失败记录仍然保留。

Runner 的二进制 HTTP、方法、JSON/form、cookie 隔离、错误脱敏，
以及本地 MinIO 构建的摘要拒绝和拉取范围可独立回归：

```bash
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s examples/pvisor/08-compose-apps -p 'test_*.py' -v
```

这些 loopback 单测只校验 runner，不替代真实应用/KVM 的业务验收。

### 数据库 socket 的历史诊断对照

默认配置保留 MariaDB `/run/mysqld`、PostgreSQL `/var/run/postgresql`
socket 目录，修复后的运行时已通过，不需要以下绕过。若复测旧运行时在根文件系统上创建 Unix socket 报
`Operation not permitted`，可另跑明确标注的应用配置对照：

```bash
python3 examples/pvisor/08-compose-apps/deploy.py \
  --binary target/debug/pvisor --case wordpress --case gitea \
  --database-sockets-in-shm
```

该选项显式设置数据库 socket 目录为 guest 已有的 tmpfs `/dev/shm`，
PostgreSQL 同时设置 `PGHOST`，并在显式对照模式下将其原本的空
`/var/run/postgresql` 目录替换为指向 `/dev/shm` 的 symlink：官方入口脚本
在执行初始化 SQL 时会清空 `PGHOST`，所以只改客户端环境不够。
默认模式不修改目录，非空目录也不会递归删除。MariaDB 不修改入口脚本。
这个选项本身只是应用配置绕过，不能作为根文件系统 Unix socket 修复证据，也不代表
Compose 已支持 `tmpfs`。默认失败和对照结果保存在不同运行目录，报告的
`database_socket_variant` 明确区分 `default` 与 `dev-shm`。

HTTP 普通请求超时为 10 秒；Gitea 首次创建带初始提交的仓库涉及 Git
子进程和磁盘写入，单独使用 60 秒上限并在报告记录实际耗时和上限。
这个上限不是性能 SLA；初次 10 秒超时的探索记录仍保留，不能据此断言
仓库创建通过或运行时有功能缺陷。

每次运行在本目录被忽略的 `.work/<唯一运行目录>/` 中建立独立私有容器存储，
保存脱敏日志和增量 `report.json`。每个场景完成后执行 `down -v`；
失败也会收集日志并尝试清理。容器记录和卷目录都必须清空；
清理失败单独记录并使 runner 返回非零，
不要将清理失败当作成功。

报告明确区分原文件校验、拉取、部署、业务断言、持久化和清理。
一个场景失败不阻止其余场景执行；任何部署/业务/清理失败使总退出码非零。
运行目录可能含应用自行生成的密钥与数据库，不应复制或公开整个 `.work/`；
共享时仅使用 runner 生成的脱敏报告与日志。

## 证据边界

以本次生成的报告为准，不预先假定应用能够启动。通过表示本地 HTTP/API
工作流及命名卷重建验收通过，不代表上游完整 Compose 配置兼容、
生产部署、TLS、邮件、SSH、故障自动重启、备份或跨平台已经验证。
