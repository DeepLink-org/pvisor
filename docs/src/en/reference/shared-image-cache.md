# Shared image cache and storage backends

`pvisor-cache` reuses OCI image content through server, filesystem, or S3 backends. In server mode, `pvisor-cache serve` prepares OCI images and generates paged indexes and content objects under `<image-store>/cache-v1/`. All three backends share immutable revisions, the object format and image reader. Clients query by `image_handle`, receive no host paths, and never access registries during file queries.

## Choose a backend

Server, filesystem and S3 caches use [shared image cache v1](../design/shared-image-cache-storage.md): independent per-image meta, shared data, and binary paged file tables/indexes. This is the sole format implementation.

Server, filesystem, and S3 caches share the same prepare/list/stat/read interface and VM configuration. Filesystem and S3 are direct storage backends and require no cache serve process.

| Backend | Configuration | Use case |
|---|---|---|
| `server` (default) | PVISOR_CACHE_SERVER=unix://... or loopback tcp://... | Reuse an extracted OCI store on one host or an existing central service |
| `filesystem` | PVISOR_CACHE_BACKEND=filesystem and PVISOR_CACHE_LOCATION=/absolute/directory | Multiple local processes or a shared disk/NFS cache |
| `s3` | PVISOR_CACHE_BACKEND=s3 and PVISOR_CACHE_LOCATION=s3://BUCKET/PREFIX | Share images across machines through object storage without a cache service |

CLI --backend, --location, and --image-store override environment values and work before or after a subcommand. --read-only or PVISOR_CACHE_READ_ONLY=true makes the shared backend read-only: preparing an unpublished image or requesting --refresh fails explicitly without registry access or backend writes. Readers may still write their own local acceleration cache.

### Filesystem: publish once, read from independent processes

```sh
pvisor-cache --backend filesystem --location /mnt/pvisor-cache \
  --image-store /tmp/pvisor-publish publish alpine:latest
pvisor-cache --backend filesystem --location /mnt/pvisor-cache \
  --read-only prepare alpine:latest
pvisor-cache --backend filesystem --location /mnt/pvisor-cache \
  --read-only read pvisor-v1:YOUR_IMAGE_KEY:linux-amd64:YOUR_REVISION etc/os-release
```

--location selects the new shared cache format; --image-store is local OCI download/extraction staging. These are separate directories. Existing PVISOR_IMAGE_STORE can also be reused. Reads do not depend on staging after publication, so CI can use task-local staging. Shared objects are private by default; administrators must grant read permissions for cross-UID shared disks. Read-only mode does not create a missing shared directory.

### S3: a writable publisher and read-only consumers

```sh
export AWS_DEFAULT_REGION=ap-southeast-1
# Supply AWS credentials through environment variables or workload roles.
export PVISOR_CACHE_BACKEND=s3
export PVISOR_CACHE_LOCATION=s3://your-bucket/pvisor-cache
pvisor-cache --image-store /tmp/pvisor-publish publish alpine:latest

# Native cache consumers only need GetObject access to this prefix.
export PVISOR_CACHE_READ_ONLY=true
pvisor-cache prepare alpine:latest
pvisor-cache read pvisor-v1:YOUR_IMAGE_KEY:linux-amd64:YOUR_REVISION etc/os-release
pvisor run --executor vm --rootfs image=alpine:latest -- /bin/sh
```

Create the bucket first; pVisor does not create buckets. Both publish and writable prepare need s3:GetObject and s3:PutObject to observe HEAD CAS tokens and verify existing immutable objects. Readers need only `s3:GetObject`. Normal operations require neither `ListBucket` nor `DeleteObject`. Storage connections remain on the host. New cache configuration and implicit AWS_* storage credentials are not projected into the guest; explicitly supplied workload environment remains the caller's decision.

S3 uses SigV4 and HTTPS by default. Credentials support AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, optional AWS_SESSION_TOKEN, and EC2, ECS, and Web Identity workload roles; the [object_store S3 provider](https://docs.rs/object_store/0.13.2/object_store/aws/struct.AmazonS3Builder.html) handles fetching and renewal. Shared ~/.aws profile/SSO files are not read directly; supply exported environment credentials or use workload roles. Set AWS_DEFAULT_REGION or AWS_REGION. S3-compatible services can use AWS_ENDPOINT (or AWS_ENDPOINT_URL_S3); the HTTP example below is for a local test service.

```sh
export AWS_ENDPOINT=http://127.0.0.1:9000
export AWS_ALLOW_HTTP=true
export AWS_ACCESS_KEY_ID=YOUR_ACCESS_KEY
export AWS_SECRET_ACCESS_KEY=YOUR_SECRET_KEY
export PVISOR_CACHE_BACKEND=s3
export PVISOR_CACHE_LOCATION=s3://your-bucket/pvisor-cache
pvisor-cache prepare alpine:latest
```

### Image splitting and upload tool

`pvisor-cache publish IMAGE` is the explicit publishing command. It pulls the selected platform's OCI manifest and layers from a registry, applies layers and whiteouts locally, walks the merged image filesystem, and uploads the file index and content blocks. Reuse existing local OCI staging through `--image-store` or `PVISOR_IMAGE_STORE`; staging can be removed after upload.

```sh
PVISOR_CACHE_READ_ONLY=false pvisor-cache publish alpine:latest \
  --backend s3 --location s3://your-bucket/pvisor-cache \
  --architecture amd64 --image-store /tmp/pvisor-publish
```

`--architecture` accepts `amd64` and `arm64`, defaulting to the host architecture. `--refresh` rechecks the registry; without it, fresh local prepared-image records can be reused. `publish` always performs splitting and upload, without skipping based on a remote tag record. Conditional creation reuses existing immutable content objects and uploads missing blocks again. Read-only mode and the server backend reject publication. On success, JSON output includes the manifest digest, architecture, COMMIT digest in `metadata_generation`, and file count/logical-byte totals in `totals`. Output also includes image_handle=pvisor-v1:<image-key>:<platform>:<revision-hex>, which list/stat/read uses for queries; digest records manifest provenance.

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
│       │   ├── files.bin
│       │   ├── contents.bin
│       │   ├── index.bin
│       │   ├── objects.bin
│       │   ├── checksums.bin
│       │   └── COMMIT.json
│       └── uploads/<upload-id>/
│           ├── plan.json
│           └── progress.json
└── data/sha256/<first-two-hex>/<next-two-hex>/<full-object-hex>
```

Each file starts at offset 0 and is split independently into raw chunks of at most 1 MiB, without packing neighboring small files. Identical files across paths, modes, or images reuse data objects. File tables, directory indexes, and content descriptors belong to their own revision. Paths/symlinks retain raw bytes, hard links share inode identity, and the existing attribute scope is preserved without adding xattrs.

### Layout and request costs

Each binary metadata object is limited to 64 MiB, and control JSON/checksum catalogs to 1 MiB each, with limits of 200,000 file entries and 500,000 content chunk descriptors. S3-compatible services must support Range GET, If-None-Match conditional creation, and If-Match conditional updates.

Publishing writes and verifies shared content, per-image metadata, and COMMIT before CAS-updating its own platform HEAD. Only one publisher can commit against a given old HEAD; conflicts fail explicitly rather than blindly overwriting. Corrupt existing immutable objects cannot be reused or overwritten while other images may reference them. Failures can leave unreferenced revisions/objects. Upload completion records remain in uploads for offline maintenance.

Startup fetches control objects, a compact checksum catalog, headers and root pages without parsing the complete file list. Lookup/readdir fetch 64 KiB index pages on demand; read fetches only related descriptors and data. Verified pages/data have bounded memory caches and persistent storage under <user-cache>/pvisor/cache-v1/objects/<location-hash>/. Corrupt local caches are refetched. Remote corruption, absence, or authorization errors fail without zero filling or registry fallback.

Writable mode reuses mutable-tag HEADs for five minutes; publishers update with --refresh. Read-only mode always uses published HEADs without registry queries. Tasks pin returned image_handles, and older revisions remain readable while retained. IMAGE@sha256:… is an independent reference whose meta must be published separately; a tag's returned image_handle already pins that version.

Actual S3 traffic includes control objects, metadata pages, and complete data chunks. TUI Transferred only measures logical file bytes returned by the cache API, not billed traffic. Automatic GC/quotas are not implemented. Disable relevant jobs/publications and confirm readers exited before deleting meta/data; never delete shared objects based solely on age.

## Code layout

Implementation lives in `crates/pvisor/src/image/cache/`:

The independent `pvisor-cache` entry point and argument parsing live in `crates/pvisor-cli/src/bin/pvisor-cache.rs` and `crates/pvisor-cli/src/cli/cache.rs`. The daemon does not own cache preparation, publication or serving.

```text
cache/
├── mod.rs              # Public entry and module assembly
├── protocol.rs         # Request/response types, framing and content hashes
├── transport.rs        # Unix/TCP endpoints, streams and timeouts
├── client.rs           # Backend discovery and validated requests
├── config.rs           # Shared CLI/executor backend configuration
├── storage.rs          # Filesystem and S3 object I/O
├── portable.rs         # Direct storage format, indexes, range reads
├── portable/           # Binary tables, publication and regression tests
├── server.rs           # Authentication, queues and shared image reader
├── server/tests.rs     # Protocol and server/direct-storage parity tests
├── source.rs           # Confined OCI source inspection during publication
├── source/             # Publication-source metadata and integrity tests
├── backend.rs          # Transport-neutral metadata, block reads and bounded caches
├── direct.rs           # VM lower metadata projection and runner attachment
├── network.rs          # Pinned read-only host access for isolated runners
├── lazy.rs             # Host FUSE adapter
├── lazy/
│   └── tests.rs        # Lazy filesystem and cache reuse tests
└── progress.rs         # Image totals and loading/transfer progress
```

`image/oci.rs` owns registry resolution, prepared records, blob validation and layer extraction. Local loading/cache servers share `ImageStore`. External callers keep the exported `cache` API; protocol/transport helpers stay internal.

## Usage

```sh
# 终端 1：前台服务端，使用默认的按用户 Unix socket 与 OCI 存储
pvisor-cache serve

# 终端 2：使用同一个默认 socket
pvisor-cache prepare alpine:latest
# 即使处于五分钟 tag 缓存窗口内也强制刷新 registry：
pvisor-cache prepare alpine:latest --refresh
# 从 JSON 结果复制 image_handle：
pvisor-cache list pvisor-v1:YOUR_IMAGE_KEY:linux-amd64:YOUR_REVISION
pvisor-cache stat pvisor-v1:YOUR_IMAGE_KEY:linux-amd64:YOUR_REVISION etc/os-release
pvisor-cache read pvisor-v1:YOUR_IMAGE_KEY:linux-amd64:YOUR_REVISION etc/os-release
```

`PVISOR_CACHE_SERVER` selects the endpoint for client/server; `pvisor-cache serve --listen` overrides it on the server. The default is `unix://<dirs::cache_dir()>/pvisor/cache.sock`:

- macOS: `~/Library/Caches/pvisor/cache.sock`
- Linux: `$XDG_CACHE_HOME/pvisor/cache.sock`, usually `~/.cache/pvisor/cache.sock`

Use `--image-store DIR` or `PVISOR_IMAGE_STORE` for existing OCI storage. The server does not start automatically.

## Automatic VM lazy loading

When no filesystem/S3 backend is selected and `pvisor run --executor vm --rootfs image=IMAGE -- COMMAND` prepares an OCI image, it probes the default socket with a two-second `ping` handshake. A compatible server enables lazy loading automatically. Missing/refused sockets (including stale sockets) use local OCI preparation. Authentication, protocol and timeout errors are reported rather than bypassed silently.

An explicit `PVISOR_CACHE_SERVER` requires the service. `PVISOR_CACHE_SERVER=off` forces local preparation. Explicit directory rootfs and native containers retain their behavior.

VM clients attach an immutable read-only lower directly to the virtio-fs service, retaining their writable upper without an intermediate host FUSE mount. Host tools retain the FUSE adapter over the same backend. Metadata is fetched on demand and cached in memory for the backend lifetime. `metadata_generation` is the revision COMMIT digest. Verified attributes and directory pages can persist across runs; rebuilding OCI extraction does not change a published revision.

Content uses 1 MiB blocks at `<user-cache>/pvisor/blocks/<endpoint-hash>/<read-handle-hash>/`, keyed by file/block. `<user-cache>` is `~/Library/Caches` on macOS and `$XDG_CACHE_HOME` (usually `~/.cache`) on Linux. This is independent of `--image-store`/`PVISOR_IMAGE_STORE`. Small files use one unpadded block; large files fetch only accessed blocks.

Each backend retains validated content in a per-file memory cache capped at 64 MiB/4096 blocks with FIFO eviction. Hot reads copy the requested slice without reopening/rehashing disk blocks. Memory misses revalidate disk content; disk corruption cannot alter already validated in-memory bytes. Validated new blocks publish atomically and use file locks for local process sharing; corrupt blocks are fetched again. VMs maintain a private metadata projection with sparse placeholders, but guest READ uses backend content instead of placeholder holes. Kernel readahead may fetch adjacent bytes; copy-up and file digests require complete originals, and complete checkpoints or self-contained exports populate the full tree. Ordinary lazy reads do not extract the complete image.

Direct backends stay attached until VM completion, then release their private projections; host FUSE mounts remain until their users exit. Cached blocks remain. Cached content survives service failure, but missing blocks return I/O errors. Digest/endpoint stay fixed for a Run with no mid-run registry fallback. Cache endpoint/token are removed from implicitly inherited guest environment.

Before responding to `prepare`, the server still fully prepares an uncached image. This is client-side lazy loading, not lazy OCI layer extraction on the server. FUSE adapters and existing virtio-fs workers handle requests synchronously, so a miss can delay unrelated filesystem requests. There is no explicit vCPU pause. Disk quotas/eviction, original OCI xattrs and asynchronous virtio-fs completion are outside this implementation.

The public blocking Rust client is `pvisor::cache::CacheClient::from_env()`. Explicit `pvisor-cache prepare/list/stat/read` commands do not use the VM's local fallback policy.

Unix sockets use 0600 permissions and require the same effective user at both ends. A lock prevents duplicate servers. Restart reclaims stale sockets, never ordinary files, symlinks or active listeners. Place the socket in a server-owned directory. Ctrl-C may leave a stale socket; manual cleanup is unnecessary.

For remote servers, use authenticated loopback TCP through SSH:

```sh
# 服务端：通过你的密钥管理/命令行设置一个强共享密钥。
export PVISOR_CACHE_TOKEN='YOUR_RANDOM_SECRET'
pvisor-cache serve --listen tcp://127.0.0.1:7447

# 客户端机器上，保持该隧道运行：
ssh -N -L 7447:127.0.0.1:7447 your-server

# 客户端 shell，使用同一密钥：
export PVISOR_CACHE_TOKEN='YOUR_RANDOM_SECRET'
export PVISOR_CACHE_SERVER=tcp://127.0.0.1:7447
pvisor-cache prepare alpine:latest
```

TCP requires a nonempty token and literal loopback IP endpoints. There is no built-in TLS; use SSH encryption. Tokens grant all cache operations, including preparing new images. This is a trusted shared service rather than a public multitenant API. Unix clients must also supply tokens when configured on the server.

## TUI transfer statistics

With `--tui`, lazy-image Runs display file counts/sizes for `Cached` (local cache reads), `Transferred` (received from local or remote image servers) and `Total` (complete image). Narrow bars abbreviate these to `C / X / T`; Overview shows all three with exact bytes. Log records validated block transfers, paths, bytes and cumulative Run totals.

Status/Overview also show local cache reads: distinct file paths and cumulative bytes served from validated memory/disk caches, including repeated reads. Only requested slices count, not internally read full 1 MiB blocks. These are separate from downloads and exclude host/guest kernel page-cache hits. The first local cache hit per file appears as `no download`. Directory listings fetch metadata without counting as content reads. Startup/I/O diagnostics go to Log rather than guest terminal, or stderr without TUI.

Downloads count validated content received from the cache server during this Run, excluding local hits, protocol metadata and guest networking. Partially downloaded files count once; repeated transfers add bytes again. A warm Run can therefore show zero downloads while using the image.

Totals count regular file paths and uncompressed logical sizes in the server's extracted image, including empty files and each hard-link name, excluding directories/symlinks. A metadata-only scan caches totals by manifest digest. Totals describe the full image rather than compressed OCI layers. Missing totals display `?`.

## Wire format

Each connection carries one request/response and closes. A frame is a four-byte unsigned big-endian JSON byte length followed by UTF-8 JSON, capped at 1 MiB. A successful `read` frame is followed by exactly `length` raw bytes. Other responses have no binary body. There are no unsolicited messages or compression.

Request envelope:

```json
{"version":1,"token":null,"request":{"op":"read","digest":"pvisor-v1:IMAGE_KEY:linux-amd64:REVISION","path":[101,116,99,47,111,115,45,114,101,108,101,97,115,101],"offset":0,"length":1048576}}
```

Paths/names use JSON arrays of Unix filename bytes, preserving non-UTF-8 names. Paths are relative to image root; an empty path means root. Absolute paths, upward traversal and NUL are rejected. Symlinks are metadata; server path resolution never follows them. Guest traversal resolves symlinks inside the guest tree.

| `op` | Fields | Response `status` |
| --- | --- | --- |
| `ping` | None | `ready` (protocol v1) |
| `prepare` | `image`, `architecture` (`amd64`/`arm64`), optional `refresh` (default false) | `prepared`: `digest`, `architecture`, `env`, `entrypoint`, `cmd`, optional `totals` (`files`, `bytes`), required `metadata_generation` and `image_handle` |
| `list` | `digest`, `path`, `offset` (zero-based entry index) | `entries`: sorted `names`, required aligned `metadata`, `next_offset` (null when complete) |
| `stat` | `digest`, `path` | `metadata`: `kind`, `size`, `mode`, `uid`, `gid`, `inode`, `nlink`, `mtime`, `mtime_nsec`, `target` |
| `read` | `digest`, `path`, `offset` (bytes), `length` (1..1048576) | `data`: `length`, `sha256`, then raw bytes |

`prepare` requests a Linux image for the client's architecture, independent of server architecture. Successful records persist at `<image-store>/metadata/prepared-v1/` with platform digest/launch configuration. Mutable tags reuse records for five minutes; immutable digests do not expire while the extracted root exists. `pvisor-cache prepare IMAGE --refresh` (`refresh: true`) forces registry resolution. Failed refresh returns an error while retaining previous records. Registry requests have a 10-second connect and 300-second total timeout. Expired tags never silently fall back to stale data. Missing/corrupt records or missing roots are prepared again.

Reference/architecture locks cover resolution/preparation; concurrent callers recheck/reuse the first successful result. Preparation may populate uncached images and retains existing digest extraction locks. `read`/`stat`/`list` require published immutable image handles and never pull images implicitly.

Directory pages include stat-equivalent attributes, avoiding one request per child. Pages contain at most 256 entries, shrinking to fit the JSON frame limit including long byte-array names/link targets. Directory attributes are required; name-only responses are rejected. Persisted pages retain attributes across mounts.

`kind` is `file`, `directory`, `symlink` or `special`. `mode` contains Unix type/permission bits; `target` contains link bytes or null. Attributes reflect the server's extracted filesystem. v1 does not reconstruct original tar ownership, provide xattrs or define portable inode IDs across servers. Only regular files are readable. Short reads, including zero bytes, indicate EOF. Clients must verify body length and SHA-256 before caching. Hashes detect transfer corruption rather than independently proving correctness against an untrusted server; server/storage are trusted.

Error frame:

```json
{"status":"error","code":"not_found","message":"..."}
```

Codes are `not_found`, `permission_denied` and `request_failed` (including invalid arguments, unsupported versions and authentication failures). Framing errors may disconnect. Early close, truncated bodies and bad checksums are failures, never missing files or zero-filled content. Messages are explanatory, not machine-stable.

Server reads use the same bounded image, metadata-page and content-object caches as filesystem/S3 reads. OCI source inspection and its directory cache run during publication, rather than on guest file requests.

There are 16 request/file workers and up to 16 queued connections; excess connections close and clients may retry. Authenticated prepare uses a separate two-worker pool with 16 queued requests; full queues return explicit busy errors. Registry/extraction work does not occupy file workers. Request reads have a five-second inactivity timeout; response reads/writes retain 300 seconds; TCP connects have 10 seconds. Long preparation may outlive a disconnected client; retry is safe. Shutdown does not gracefully cancel individual OCI downloads. Existing image storage governs registry limits/cache eviction; v1 adds no quotas or eviction.

There are no vCPU pause/resume messages. VM requests are handled inside the isolated runner; filesystem and Unix socket caches are accessed directly. Linux TCP/S3 downloads use a private Unix socket to a host access process, preserving runner network isolation. It permits only the bound immutable image's stat/list/read, rejecting prepare, open and other images. Credential descriptors stay in the hidden private owner, and the access process terminates during VM teardown. Host FUSE continues fetching content on the host.

## Backend validation

`just test pvisor` includes server compatibility, independent filesystem/S3 CLI processes, read-only access, corruption refusal, and configuration precedence. The local S3 fixture independently verifies SigV4, including temporary session tokens, without real accounts, public requests, or an external daemon.

On Linux x86_64 with KVM and the static musl guest target, run the real VM acceptance check:

```sh
cargo nextest run --locked -p pvisor --test cache_backends --run-ignored ignored-only
```

This starts VMs from both backends, validates guest file contents after deleting publisher staging, and verifies that inheriting the host environment does not leak storage credentials.
