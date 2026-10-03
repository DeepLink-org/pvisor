# Shared image cache and storage backends

`pvisor cache` reuses OCI image content through server, filesystem, or S3 backends. In server mode, `pvisor cache serve` exposes existing OCI image storage as a read-only file service. Clients never receive host filesystem paths. Images are prepared once on the server and addressed by the resolved platform manifest SHA-256 digest. Existing storage validates blobs, applies layers and handles whiteouts. File queries never access registries.

## Choose a backend

See [shared image cache storage format v1](../design/shared-image-cache-storage.md) for directory trees, object fields, file-to-block mappings, and publication consistency, including complete readable example objects.

The successor [v2 layout isolates per-image meta and shares sharded data](../design/shared-image-cache-storage-v2.md). Publishers/readers do not yet implement it; commands below still use v1.

Server, filesystem, and S3 caches share the same prepare/list/stat/read interface and VM configuration. Filesystem and S3 are direct storage backends and require no cache serve process.

| Backend | Configuration | Use case |
|---|---|---|
| `server` (default) | PVISOR_CACHE_SERVER=unix://... or loopback tcp://... | Reuse an extracted OCI store on one host or an existing central service |
| `filesystem` | PVISOR_CACHE_BACKEND=filesystem and PVISOR_CACHE_LOCATION=/absolute/directory | Multiple local processes or a shared disk/NFS cache |
| `s3` | PVISOR_CACHE_BACKEND=s3 and PVISOR_CACHE_LOCATION=s3://BUCKET/PREFIX | Share images across machines through object storage without a cache service |

CLI --backend, --location, and --image-store override environment values and work before or after a subcommand. --read-only or PVISOR_CACHE_READ_ONLY=true makes the shared backend read-only: preparing an unpublished image or requesting --refresh fails explicitly without registry access or backend writes. Readers may still write their own local acceleration cache.

### Filesystem: publish once, read from independent processes

```sh
pvisor cache --backend filesystem --location /mnt/pvisor-cache \
  --image-store /tmp/pvisor-publish publish alpine:latest
pvisor cache --backend filesystem --location /mnt/pvisor-cache \
  --read-only prepare alpine:latest
pvisor cache --backend filesystem --location /mnt/pvisor-cache \
  --read-only read sha256:YOUR_MANIFEST_DIGEST etc/os-release
```

--location selects the new shared cache format; --image-store is local OCI download/extraction staging. These are separate directories. Existing PVISOR_IMAGE_STORE can also be reused. Reads do not depend on staging after publication, so CI can use task-local staging. Shared objects are private by default; administrators must grant read permissions for cross-UID shared disks. Read-only mode does not create a missing shared directory.

### S3: a writable publisher and read-only workers

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

Create the bucket first; pVisor does not create buckets. `publish` uploads objects to the prefix and needs `s3:PutObject`. Writable `prepare` also needs `s3:GetObject` because it checks the cache before publishing a miss. Readers need only `s3:GetObject`. Normal operations require neither `ListBucket` nor `DeleteObject`. Storage connections remain on the host. New cache configuration and implicit AWS_* storage credentials are not projected into the guest; explicitly supplied workload environment remains the caller's decision.

S3 uses SigV4 and HTTPS by default. Credentials support AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, optional AWS_SESSION_TOKEN, and EC2, ECS, and Web Identity workload roles; the [object_store S3 provider](https://docs.rs/object_store/0.13.2/object_store/aws/struct.AmazonS3Builder.html) handles fetching and renewal. Shared ~/.aws profile/SSO files are not read directly; supply exported environment credentials or use workload roles. Set AWS_DEFAULT_REGION or AWS_REGION. S3-compatible services can use AWS_ENDPOINT (or AWS_ENDPOINT_URL_S3); the HTTP example below is for a local test service.

```sh
export AWS_ENDPOINT=http://127.0.0.1:9000
export AWS_ALLOW_HTTP=true
export AWS_ACCESS_KEY_ID=YOUR_ACCESS_KEY
export AWS_SECRET_ACCESS_KEY=YOUR_SECRET_KEY
export PVISOR_CACHE_BACKEND=s3
export PVISOR_CACHE_LOCATION=s3://your-bucket/pvisor-cache
pvisor cache prepare alpine:latest
```

### Image splitting and upload tool

`pvisor cache publish IMAGE` is the explicit publishing command. It pulls the selected platform's OCI manifest and layers from a registry, applies layers and whiteouts locally, walks the merged image filesystem, and uploads the file index and content blocks. Reuse existing local OCI staging through `--image-store` or `PVISOR_IMAGE_STORE`; staging can be removed after upload.

```sh
PVISOR_CACHE_READ_ONLY=false pvisor cache publish alpine:latest \
  --backend s3 --location s3://your-bucket/pvisor-cache \
  --architecture amd64 --image-store /tmp/pvisor-publish
```

`--architecture` accepts `amd64` and `arm64`, defaulting to the host architecture. `--refresh` rechecks the registry; without it, fresh local prepared-image records can be reused. `publish` always performs splitting and upload, without skipping based on a remote tag record. Conditional creation reuses existing immutable content objects and uploads missing blocks again. Read-only mode and the server backend reject publication. On success, JSON output includes the manifest digest, architecture, index digest in `metadata_generation`, and file count/logical-byte totals in `totals`. Readers use these indexes to access files by path.

```text
s3://your-bucket/pvisor-cache/
└── v1/
    ├── format
    ├── refs/<reference-and-architecture-hash>.json
    ├── images/<manifest-sha256>.json
    ├── indexes/<index-sha256>.json
    └── blobs/<content-sha256>
```

This file storage structure combines indexed paths, modes, hard links, symlinks, and content spans. S3 content objects contain blocks of at most 1 MiB. A large file can use several objects, and several small files can share one object. The index maps file paths to content blocks, so readers do not download or extract OCI layers again.

### Layout and request costs

A direct-backend image index is limited to 64 MiB, 200,000 paths, and 500,000 content spans; larger indexes fail explicitly. Compatible object services must support atomic PUT and If-None-Match conditional creation.

Both direct backends use the v1/ format: refs/ contains image-reference/architecture records, images/ maps manifest digests to indexes, indexes/ holds immutable metadata and file-span indexes, and blobs/ holds SHA-256-addressed content. Paths and symlinks preserve Unix bytes, hard links share inode identity, and modes/Linux ownership follow OCI extraction rules.

Publishing packs small files into blocks of at most 1 MiB; large files can span blocks. Content is deduplicated by hash. All blocks and indexes are written before atomically publishing references. Concurrent publishers conditionally create immutable objects, so readers never see partial uploads. Failed publication may leave unreferenced blocks but does not publish a reference to an unfinished upload.

Reads fetch a reference and index, followed only by blocks intersecting the requested range. Directory queries use the index alone. Bounded client memory caches and a persistent local cache under <user-cache>/pvisor/cache-v1/objects/<location-hash>/ reuse validated indexes and packed blocks. Corrupt local objects are refetched. Corrupt, missing, or denied remote objects fail the operation without zero filling or silently falling back to the registry. Existing VM file-block and metadata caches still apply.

Writable mode caches mutable-tag records for five minutes; publishers explicitly update them with --refresh. Read-only mode uses published records beyond that window without registry access. Refresh from a publisher to update workers, whose tasks then pin the returned manifest digest. IMAGE@sha256:... pins a version.

Actual S3 traffic includes indexes and complete packed blocks; a small-file read can fetch neighboring file bytes. Existing TUI Transferred counts logical file bytes returned by the cache interface rather than S3 GETs or billed traffic, so it cannot estimate object-storage bills. Shared-block automatic GC/quotas are not included. Retire independent prefixes as groups after confirming no tasks use them. Expiring old blobs alone can delete content still used by newer references.

## Code layout

Implementation lives in `crates/pvisor/src/image/cache/`:

```text
cache/
├── mod.rs              # Public entry and module assembly
├── cli.rs              # pvisor cache subcommands
├── protocol.rs         # Request/response types, framing and content hashes
├── transport.rs        # Unix/TCP endpoints, streams and timeouts
├── client.rs           # Backend discovery and validated requests
├── config.rs           # Shared CLI/executor backend configuration
├── storage.rs          # Filesystem and S3 object I/O
├── portable.rs         # Direct storage format, indexes, range reads
├── portable/           # Packing/publication and regression tests
├── server.rs           # Authentication, work queues and confined file access
├── server/
│   ├── metadata.rs     # Server metadata and directory LRU caches
│   └── tests.rs        # Protocol/confinement/client-server tests
├── lazy.rs             # FUSE mounts, block cache and client metadata cache
├── lazy/
│   └── tests.rs        # Lazy filesystem and cache reuse tests
└── progress.rs         # Image totals and loading/transfer progress
```

`image/oci.rs` owns registry resolution, prepared records, blob validation and layer extraction. Local loading/cache servers share `ImageStore`. External callers keep the exported `cache` API; protocol/transport helpers stay internal.

## Usage

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

`PVISOR_CACHE_SERVER` selects the endpoint for client/server; `cache serve --listen` overrides it on the server. The default is `unix://<dirs::cache_dir()>/pvisor/cache.sock`:

- macOS: `~/Library/Caches/pvisor/cache.sock`
- Linux: `$XDG_CACHE_HOME/pvisor/cache.sock`, usually `~/.cache/pvisor/cache.sock`

Use `--image-store DIR` or `PVISOR_IMAGE_STORE` for existing OCI storage. The server does not start automatically.

## Automatic VM lazy loading

When no filesystem/S3 backend is selected and `pvisor run --executor vm --rootfs image=IMAGE -- COMMAND` prepares an OCI image, it probes the default socket with a two-second `ping` handshake. A compatible server enables lazy loading automatically. Missing/refused sockets (including stale sockets) use local OCI preparation. Authentication, protocol and timeout errors are reported rather than bypassed silently.

An explicit `PVISOR_CACHE_SERVER` requires the service. `PVISOR_CACHE_SERVER=off` forces local preparation. Explicit directory rootfs and native containers retain their behavior.

The client mounts an immutable read-only FUSE lower (macFUSE FSKit on macOS, FUSE on Linux), retaining the existing writable VM upper. Metadata is fetched on demand and cached in memory for the mount. When the server advertises `metadata_generation`, validated stat responses (including absent paths) and directory pages also persist at `<user-cache>/pvisor/metadata/v1/<endpoint-hash>/<manifest-digest>/<generation-hash>/`. They survive VM exit; corrupt entries are fetched again. Older servers without generation remain memory-only. Generation includes server root identity/change time, invalidating metadata with old host inode numbers after root reconstruction. Prepared roots must remain immutable; in-place modification is unsupported.

Content uses 1 MiB blocks at `<user-cache>/pvisor/blocks/<endpoint-hash>/<manifest-digest>/`, keyed by file/block. `<user-cache>` is `~/Library/Caches` on macOS and `$XDG_CACHE_HOME` (usually `~/.cache`) on Linux. This is independent of `--image-store`/`PVISOR_IMAGE_STORE`. Small files use one unpadded block; large files fetch only accessed blocks.

Each mount retains validated content in a per-file memory cache capped at 64 MiB/4096 blocks with FIFO eviction. Hot reads copy the requested slice without reopening/rehashing disk blocks. Memory misses revalidate disk content; disk corruption cannot alter already validated in-memory bytes. Validated new blocks publish atomically and use file locks for local process sharing; corrupt blocks are fetched again. There are no sparse placeholder files. Kernel readahead may fetch adjacent bytes; copy-up may read entire files. The client does not extract the full image.

The FUSE mount remains until VM completion, then unmounts; cached blocks remain. Cached content survives service failure, but missing blocks return I/O errors. Digest/endpoint stay fixed for a Run with no mid-run registry fallback. Cache endpoint/token are removed from implicitly inherited guest environment.

Before responding to `prepare`, the server still fully prepares an uncached image. This is client-side lazy loading, not lazy OCI layer extraction on the server. FUSE adapters and existing virtio-fs workers handle requests synchronously, so a miss can delay unrelated filesystem requests. There is no explicit vCPU pause. Disk quotas/eviction, original OCI xattrs and asynchronous virtio-fs completion are outside this implementation.

The public blocking Rust client is `pvisor::cache::CacheClient::from_env()`. Explicit `cache prepare/list/stat/read` commands do not use the VM's local fallback policy.

Unix sockets use 0600 permissions and require the same effective user at both ends. A lock prevents duplicate servers. Restart reclaims stale sockets, never ordinary files, symlinks or active listeners. Place the socket in a server-owned directory. Ctrl-C may leave a stale socket; manual cleanup is unnecessary.

For remote servers, use authenticated loopback TCP through SSH:

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

TCP requires a nonempty token and literal loopback IP endpoints. There is no built-in TLS; use SSH encryption. Tokens grant all cache operations, including preparing new images. This is a trusted shared service rather than a public multitenant API. Unix clients must also supply tokens when configured on the server.

## TUI transfer statistics

With `--tui`, lazy-image Runs display file counts/sizes for `Cached` (local cache reads), `Transferred` (received from local or remote image servers) and `Total` (complete image). Narrow bars abbreviate these to `C / X / T`; Overview shows all three with exact bytes. Log records validated block transfers, paths, bytes and cumulative Run totals.

Status/Overview also show local cache reads: distinct file paths and cumulative bytes served from validated memory/disk caches, including repeated reads. Only requested slices count, not internally read full 1 MiB blocks. These are separate from downloads and exclude host/guest kernel page-cache hits. The first local cache hit per file appears as `no download`. Directory listings fetch metadata without counting as content reads. Startup/I/O diagnostics go to Log rather than guest terminal, or stderr without TUI.

Downloads count validated content received from the cache server during this Run, excluding local hits, protocol metadata and guest networking. Partially downloaded files count once; repeated transfers add bytes again. A warm Run can therefore show zero downloads while using the image.

Totals count regular file paths and uncompressed logical sizes in the server's extracted image, including empty files and each hard-link name, excluding directories/symlinks. A metadata-only scan caches totals by manifest digest. Totals describe the full image rather than compressed OCI layers. Older servers without totals display `?`.

## Wire format

Each connection carries one request/response and closes. A frame is a four-byte unsigned big-endian JSON byte length followed by UTF-8 JSON, capped at 1 MiB. A successful `read` frame is followed by exactly `length` raw bytes. Other responses have no binary body. There are no unsolicited messages or compression.

Request envelope:

```json
{"version":1,"token":null,"request":{"op":"read","digest":"sha256:...","path":[101,116,99,47,111,115,45,114,101,108,101,97,115,101],"offset":0,"length":1048576}}
```

Paths/names use JSON arrays of Unix filename bytes, preserving non-UTF-8 names. Paths are relative to image root; an empty path means root. Absolute paths, upward traversal and NUL are rejected. Symlinks are metadata; server path resolution never follows them. Guest traversal resolves symlinks inside the guest tree.

| `op` | Fields | Response `status` |
| --- | --- | --- |
| `ping` | None | `ready` (protocol v1) |
| `prepare` | `image`, `architecture` (`amd64`/`arm64`), optional `refresh` (default false) | `prepared`: `digest`, `architecture`, `env`, `entrypoint`, `cmd`, optional `totals` (`files`, `bytes`), optional `metadata_generation` |
| `list` | `digest`, `path`, `offset` (zero-based entry index) | `entries`: sorted `names`, optional aligned `metadata`, `next_offset` (null when complete) |
| `stat` | `digest`, `path` | `metadata`: `kind`, `size`, `mode`, `uid`, `gid`, `inode`, `nlink`, `mtime`, `mtime_nsec`, `target` |
| `read` | `digest`, `path`, `offset` (bytes), `length` (1..1048576) | `data`: `length`, `sha256`, then raw bytes |

`prepare` requests a Linux image for the client's architecture, independent of server architecture. Successful records persist at `<image-store>/metadata/prepared-v1/` with platform digest/launch configuration. Mutable tags reuse records for five minutes; immutable digests do not expire while the extracted root exists. `cache prepare IMAGE --refresh` (`refresh: true`) forces registry resolution. Failed refresh returns an error while retaining previous records. Registry requests have a 10-second connect and 300-second total timeout. Expired tags never silently fall back to stale data. Missing/corrupt records or missing roots are prepared again.

Reference/architecture locks cover resolution/preparation; concurrent callers recheck/reuse the first successful result. Preparation may populate uncached images and retains existing digest extraction locks. `read`/`stat`/`list` require prepared digests and never pull images implicitly.

Directory pages include stat-equivalent attributes, avoiding one request per child. Pages contain at most 256 entries, shrinking to fit the JSON frame limit including long byte-array names/link targets. Older name-only responses remain compatible through individual stat requests. Persisted pages retain attributes across mounts.

`kind` is `file`, `directory`, `symlink` or `special`. `mode` contains Unix type/permission bits; `target` contains link bytes or null. Attributes reflect the server's extracted filesystem. v1 does not reconstruct original tar ownership, provide xattrs or define portable inode IDs across servers. Only regular files are readable. Short reads, including zero bytes, indicate EOF. Clients must verify body length and SHA-256 before caching. Hashes detect transfer corruption rather than independently proving correctness against an untrusted server; server/storage are trusted.

Error frame:

```json
{"status":"error","code":"not_found","message":"..."}
```

Codes are `not_found`, `permission_denied` and `request_failed` (including invalid arguments, unsupported versions and authentication failures). Framing errors may disconnect. Early close, truncated bodies and bad checksums are failures, never missing files or zero-filled content. Messages are explanatory, not machine-stable.

Server memory caches share up to 4096 stat responses and 128 sorted directory indexes. Pagination reuses an index instead of rescanning/sorting. At capacity, LRU removes one entry rather than clearing the cache. Filesystem I/O runs outside the cache lock; concurrent misses may duplicate a read without blocking unrelated hits. Restart rebuilds caches lazily.

There are 16 request/file workers and up to 16 queued connections; excess connections close and clients may retry. Authenticated prepare uses a separate two-worker pool with 16 queued requests; full queues return explicit busy errors. Registry/extraction work does not occupy file workers. Request reads have a five-second inactivity timeout; response reads/writes retain 300 seconds; TCP connects have 10 seconds. Long preparation may outlive a disconnected client; retry is safe. Shutdown does not gracefully cancel individual OCI downloads. Existing image storage governs registry limits/cache eviction; v1 adds no quotas or eviction.

There are no vCPU pause/resume messages. Downloading happens in host FUSE services outside the sandboxed VM runner.

## Backend validation

`just test pvisor` includes server compatibility, independent filesystem/S3 CLI processes, read-only access, corruption refusal, and configuration precedence. The local S3 fixture independently verifies SigV4, including temporary session tokens, without real accounts, public requests, or an external daemon.

On Linux x86_64 with KVM/FUSE and the static musl guest target, run the real VM acceptance check:

```sh
cargo nextest run --locked -p pvisor --test cache_backends --run-ignored ignored-only
```

This starts VMs from both backends, validates guest file contents after deleting publisher staging, and verifies that inheriting the host environment does not leak storage credentials.
