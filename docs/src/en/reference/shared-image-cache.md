# Shared image cache and storage backends

`pvisor cache` reuses OCI image content through server, filesystem, or S3 backends. In server mode, `pvisor-daemon serve --cache` prepares OCI images and generates paged indexes and content objects under `<image-store>/pool-v2/`. All three backends share immutable revisions, the object format and image reader. Clients query by `image_handle`, receive no host paths, and never access registries during file queries.

## Local image directory {#local-image-store}

Docker-compatible commands, spawn without a selected remote backend, and the cache server share one local image directory. Its default is `~/Library/Application Support/pvisor/docker/images/` on macOS and `$XDG_DATA_HOME/pvisor/docker/images/` on Linux, usually `~/.local/share/pvisor/docker/images/`. Revisions referenced by tasks belong in persistent user data storage.

An explicit Job/cache image-store option takes precedence, followed by `PVISOR_IMAGE_STORE`. With only `PVISOR_DOCKER_HOME` set, all local entry points use its `images/`, preserving isolated storage. Embedded `Store::open(root)` always uses `root/images`, independently of environment defaults. Container state, Job records and writable layers keep their separate owners. The local image root and its control directories must be owned by the current user, private (0700) and not symlinks; incompatible existing directories fail without changing their permissions.

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

Download/extraction directories under `staging` are removed on normal success or failure; process crashes can leave residual directories. `configs` retains digest-checked original OCI configuration. All new local image publication uses `pool-v2`. Docker pull still resolves the registry, but reuses matching shared-pool digests. Build tags also publish V2 references, committing each tag independently without a multi-tag transaction. `rmi` removes container frontend catalog references without reclaiming shared content; active and stopped tasks retain fixed revisions.

Short and fully qualified Docker Hub aliases share one canonical catalog identity, including pull/build replacement, create/FROM lookup and rmi. Equivalent build tags publish once. Distinct tags and existing containers retain their pinned revisions. Catalogs containing duplicate canonical identities fail explicitly; rebuild those catalogs in a fresh store.

Local pool tags stay available offline until an explicit pull/refresh or another publication replaces them. The five-minute automatic tag refresh applies to writable filesystem/S3 backends, not the local pool.

Only current V2 image records and pool formats are accepted. Directory-root records, missing platform/binding fields and old cache layouts are rejected; no migration or compatibility reader is provided. Stop and clean up old containers through the old version, then re-pull or rebuild into a fresh store and recreate containers. Existing files are not automatically deleted.

## Choose a backend

Server, filesystem, S3 and local pools use [shared image cache V2](../design/lazy-image-v2.md): independent image metadata, compressed directory radix indexes and shared content pools. The V1 storage implementation is removed. Republish OCI sources into an empty directory/prefix; old data is never automatically deleted.

Server, filesystem, and S3 caches share the same prepare/list/stat/read interface and VM configuration. Filesystem and S3 are direct storage backends and require no cache serve process.

| Backend | Configuration | Use case |
|---|---|---|
| `server` (default) | PVISOR_CACHE_SERVER=unix://... or loopback tcp://... | Serve the host's V2 image pool or an existing central service |
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
  --read-only read pvisor-v2:YOUR_IMAGE_KEY:linux-amd64:YOUR_REVISION etc/os-release
```

`--location` selects shared storage; `--image-store` or `PVISOR_IMAGE_STORE` selects the local parent for scoped `staging/.import-*` imports. Downloaded layers, extracted roots and temporary prepared records are removed on success or error. Process crashes can leave residual directories. Reads do not depend on imports after publication. Shared objects are private by default; administrators must grant read permissions for cross-UID shared disks. Read-only mode does not create a missing shared directory.

### S3: a writable publisher and read-only consumers

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

Create the bucket first; pVisor does not create buckets. Both publish and writable prepare need s3:GetObject and s3:PutObject to observe HEAD CAS tokens and verify existing immutable objects. Readers need only `s3:GetObject`. Normal operations require neither `ListBucket` nor `DeleteObject`. Storage connections remain on the host. New cache configuration and implicit AWS_* storage credentials are not projected into the guest; explicitly supplied workload environment remains the caller's decision.

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

`pvisor cache publish IMAGE` is the explicit publishing command. It pulls the selected platform's OCI manifest and layers from a registry, applies layers and whiteouts locally, walks the merged image filesystem, and uploads the file index and content blocks. Each invocation owns a temporary import below `--image-store` or `PVISOR_IMAGE_STORE` through the end of publication. It removes raw OCI data on success and failure. Host registry TLS accepts an additional PEM CA bundle through `SSL_CERT_FILE`; unreadable or invalid bundles fail explicitly.

```sh
PVISOR_CACHE_READ_ONLY=false pvisor cache publish alpine:latest \
  --backend s3 --location s3://your-bucket/pvisor-cache \
  --architecture amd64 --image-store /tmp/pvisor-publish
```

`--architecture` accepts `amd64` and `arm64`, defaulting to the host architecture. `publish` resolves the registry on every invocation, including without `--refresh`; temporary import records are not reused between calls. Writable `prepare` reuses shared HEAD for five minutes for mutable tags, until explicit refresh for immutable digests. `publish` always performs splitting and upload, without skipping based on a remote tag record. Conditional creation reuses existing immutable content objects and uploads missing blocks again. Read-only mode and the server backend reject publication. On success, JSON output includes the manifest digest, architecture, COMMIT digest in `metadata_generation`, and file count/logical-byte totals in `totals`. Output also includes image_handle=pvisor-v2:<image-key>:<platform>:<revision-hex>, which list/stat/read uses for queries; digest records manifest provenance.

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

Small files ≤64 KiB are packed by directory into immutable compressed blobs. Indexes hold blob/frame/slot references; slot tables in data frames hold offsets. Large files split independently from offset 0 into content chunks of at most 1 MiB. Data objects contain no filesystem metadata; placements from the previous HEAD can survive rename, moves and chmod. Partially overlapping small-file sets do not guarantee physical per-file deduplication across images. Paths/symlinks retain raw bytes, hardlinks retain inode identity, and arbitrary xattrs are not added.

### Layout and request costs

Individual binary metadata objects are capped at 64 MiB and control objects at 1 MiB. Limits are 1,000,000 entries and 2,000,000 content spans; the object-size cap can be reached first. S3-compatible services must support Range GET, If-None-Match creation and If-Match updates.

Publishing writes and verifies shared content, per-image metadata, and COMMIT before CAS-updating its own platform HEAD. Only one publisher can commit against a given old HEAD; conflicts fail explicitly rather than blindly overwriting. Corrupt existing immutable objects cannot be reused or overwritten while other images may reference them. Failures can leave unreferenced revisions/objects. Upload completion records remain in uploads for offline maintenance.

Open fetches controls, root.bin and the root-directory frame without parsing the full file list. Lookup/readdir load independent compressed index frames; reads fetch relevant layout/data frames. The index LRU has a 16 MiB decoded residency limit and content a 64 MiB/4,096-entry limit. Non-pool readers may persist encoded frames under <user-cache>/pvisor/cache-v2/objects/<location-hash>/. Ordinary local-pool reads create no persistent raw image mirror. Corrupt local caches are refetched; remote absence, corruption and authentication failures fail explicitly, without zero filling or registry fallback.

Writable mode reuses mutable-tag HEADs for five minutes; publishers update with --refresh. Read-only mode always uses published HEADs without registry queries. Tasks pin returned image_handles, and older revisions remain readable while retained. IMAGE@sha256:… is an independent reference whose meta must be published separately; a tag's returned image_handle already pins that version.

Actual S3 traffic includes control objects, metadata pages, and complete data chunks. TUI Transferred only measures logical file bytes returned by the cache API, not billed traffic. Automatic GC/quotas are not implemented. Disable relevant jobs/publications and confirm readers exited before deleting meta/data; never delete shared objects based solely on age.

## Code layout

Implementation lives in `crates/pvisor/src/image/cache/`:

Cache command parsing lives in `crates/pvisor-cli/src/cli/cache.rs`. `pvisor-daemon serve --cache` owns the server entry point; storage and protocol implementations remain in the runtime.

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

`image/store.rs` owns local path selection, temporary imports and shared pull locks. `image/oci.rs` owns registry resolution, prepared records, blob validation and layer extraction. Local VM preparation, Docker commands and cache servers share `LocalStore` and the V2 pool. External callers keep the exported `cache` API; protocol/transport helpers stay internal.

## Usage

```sh
# 终端 1：前台服务端，使用默认的按用户 Unix socket 与 OCI 存储
pvisor-daemon serve --cache

# 终端 2：使用同一个默认 socket
pvisor cache prepare alpine:latest
# Explicitly refresh a locally published tag from the registry:
pvisor cache prepare alpine:latest --refresh
# 从 JSON 结果复制 image_handle：
pvisor cache list pvisor-v2:YOUR_IMAGE_KEY:linux-amd64:YOUR_REVISION
pvisor cache stat pvisor-v2:YOUR_IMAGE_KEY:linux-amd64:YOUR_REVISION etc/os-release
pvisor cache read pvisor-v2:YOUR_IMAGE_KEY:linux-amd64:YOUR_REVISION etc/os-release
```

`PVISOR_CACHE_SERVER` selects the endpoint for client/server; `pvisor-daemon serve --cache --cache-listen` overrides it on the server. The default is `unix://<dirs::cache_dir()>/pvisor/cache.sock`:

- macOS: `~/Library/Caches/pvisor/cache.sock`
- Linux: `$XDG_CACHE_HOME/pvisor/cache.sock`, usually `~/.cache/pvisor/cache.sock`

Use `--cache-image-store DIR` or `PVISOR_IMAGE_STORE` to select the server’s local image directory, shared with Docker and daemonless VM Jobs. The server does not start automatically.

## Automatic VM lazy loading

When no filesystem/S3 backend is selected and `pvisor spawn --executor vm --rootfs image=IMAGE -- COMMAND` prepares an OCI image, it probes the default socket with a two-second `ping` handshake. A compatible server enables lazy loading automatically. Missing/refused sockets (including stale sockets) prepare and read through the same local V2 pool. Authentication, protocol and timeout errors are reported rather than bypassed silently.

An explicit `PVISOR_CACHE_SERVER` requires the service. `PVISOR_CACHE_SERVER=off` forces the local V2 pool. Explicit directory rootfs and native containers retain their behavior.

VM clients attach an immutable read-only lower directly to the virtio-fs service, retaining their writable upper without an intermediate host FUSE mount. Host tools retain the FUSE adapter over the same backend. Metadata is fetched on demand and cached in memory for the backend lifetime. `metadata_generation` is the revision COMMIT digest. For socket/filesystem/S3 clients, verified attributes and directory pages can persist across runs; rebuilding OCI extraction does not change a published revision.

Socket/filesystem/S3 clients cache content in 1 MiB blocks at `<user-cache>/pvisor/blocks/<endpoint-hash>/<read-handle-hash>/`, keyed by file/block. `<user-cache>` is `~/Library/Caches` on macOS and `$XDG_CACHE_HOME` (usually `~/.cache`) on Linux. This is independent of `--image-store`/`PVISOR_IMAGE_STORE`. Small files use one unpadded block; large files fetch only accessed blocks.

Each backend retains validated content in a per-file memory cache capped at 64 MiB/4096 blocks with FIFO eviction. Hot reads copy the requested slice without reopening/rehashing disk blocks. Memory misses revalidate disk content; disk corruption cannot alter already validated in-memory bytes. Validated new blocks publish atomically and use file locks for local process sharing; corrupt blocks are fetched again. VMs maintain a private metadata projection with sparse placeholders, but guest READ uses backend content instead of placeholder holes. Kernel readahead may fetch adjacent bytes; copy-up and file digests require complete originals, and complete checkpoints or self-contained exports populate the full tree. Ordinary lazy reads do not extract the complete image.

Direct backends stay attached until VM completion, then release their private projections; host FUSE mounts remain until their users exit. Cached blocks remain. Cached content survives service failure, but missing blocks return I/O errors. Digest/endpoint stay fixed for a Run with no mid-run registry fallback. Cache endpoint/token are removed from implicitly inherited guest environment.

Before responding to `prepare`, the server fully prepares an uncached image and generates indexes/content objects in a temporary import directory, removed after success or an ordinary error. This is client-side lazy loading, not lazy OCI layer extraction on the server. FUSE adapters and existing virtio-fs workers handle requests synchronously, so a miss can delay unrelated filesystem requests. There is no explicit vCPU pause. Disk quotas/eviction, original OCI xattrs and asynchronous virtio-fs completion are outside this implementation.

The public blocking Rust client is `pvisor::cache::CacheClient::from_env()`. Explicit `pvisor cache prepare/list/stat/read` commands do not use the VM's local fallback policy.

Unix sockets use 0600 permissions and require the same effective user at both ends. A lock prevents duplicate servers. Restart reclaims stale sockets, never ordinary files, symlinks or active listeners. Place the socket in a server-owned directory. Ctrl-C may leave a stale socket; manual cleanup is unnecessary.

For remote servers, use authenticated loopback TCP through SSH:

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

TCP requires a nonempty token and literal loopback IP endpoints. There is no built-in TLS; use SSH encryption. Tokens grant all cache operations, including preparing new images. This is a trusted shared service rather than a public multitenant API. Unix clients must also supply tokens when configured on the server.

## TUI transfer statistics

With `--tui`, lazy-image Runs display file counts/sizes for `Cached` (local cache reads), `Transferred` (received from local or remote image servers) and `Total` (complete image). Narrow bars abbreviate these to `C / X / T`; Overview shows all three with exact bytes. Log records validated block transfers, paths, bytes and cumulative Run totals.

Status/Overview also show local cache reads: distinct file paths and cumulative bytes served from validated memory/disk caches, including repeated reads. Only requested slices count, not internally read full 1 MiB blocks. These are separate from downloads and exclude host/guest kernel page-cache hits. The first local cache hit per file appears as `no download`. Directory listings fetch metadata without counting as content reads. Startup/I/O diagnostics go to Log rather than guest terminal, or stderr without TUI.

Downloads count validated content received from the cache server during this Run, excluding local hits, protocol metadata and guest networking. Partially downloaded files count once; repeated transfers add bytes again. A warm Run can therefore show zero downloads while using the image.

Totals count regular file paths and uncompressed logical sizes in the server's extracted image, including empty files and each hard-link name, excluding directories/symlinks. A metadata-only scan caches totals by manifest digest. Totals describe the full image rather than compressed OCI layers. Missing totals display `?`.

## Wire format

Only protocol 2 is accepted by the server and VM bridge. A client sends one harmless Ping before its actual request; rejection or EOF fails without downgrade. Sequential reuse allows up to four pooled client connections. Disabling reuse still sends V2. Storage V2 is independent of the wire protocol. Frames use a four-byte big-endian length followed by UTF-8 JSON capped at 1 MiB. Successful read and metadata replies carry exactly the declared raw body length. Transport bodies are uncompressed (metadata objects themselves may be compressed), with mandatory length and SHA-256 validation.

Request envelope:

```json
{"version":2,"token":null,"request":{"op":"read","digest":"pvisor-v2:IMAGE_KEY:linux-amd64:REVISION","path":[101,116,99,47,111,115,45,114,101,108,101,97,115,101],"offset":0,"length":1048576}}
```

Paths/names use JSON arrays of Unix filename bytes, preserving non-UTF-8 names. Paths are relative to image root; an empty path means root. Absolute paths, upward traversal and NUL are rejected. Symlinks are metadata; server path resolution never follows them. Guest traversal resolves symlinks inside the guest tree.

| `op` | Fields | Response `status` |
| --- | --- | --- |
| `ping` | None | `ready` |
| `prepare` | `image`, `architecture` (`amd64`/`arm64`), optional `refresh` (default false) | `prepared`: `digest`, `architecture`, `env`, `entrypoint`, `cmd`, optional `totals` (`files`, `bytes`), required `metadata_generation` and `image_handle` |
| `list` | `digest`, `path`, `offset` (0 initially, then opaque continuation) | `entries`: sorted `names`, required aligned `metadata`, `next_offset` (null when complete) |
| `stat` | `digest`, `path` | `metadata`: `kind`, `size`, `mode`, `uid`, `gid`, `inode`, `nlink`, `mtime`, `mtime_nsec`, `target` |
| `read` | `digest`, `path`, `offset` (bytes), `length` (1..1048576) | `data`: `length`, `sha256`, then raw bytes |
| `open` | `handle`, `architecture` | `prepared` |
| `metadata` | `handle`, `object_name`, `offset`, `length` | `data` + bytes |

`prepare` requests a Linux image for the client's architecture, independent of server architecture. The published V2 HEAD contains the platform digest and launch configuration; no persistent raw prepared-image record is required. Local pool tags remain available offline until explicit refresh or replacement. Writable filesystem/S3 backends reuse mutable HEAD for five minutes and immutable digest HEAD until refresh. `--refresh` (`refresh: true`) forces registry resolution. Failed imports preserve the previous HEAD and remove their temporary directories. Registry connections have a 10-second connect timeout; blocking requests and body reads have a 30-second timeout. A transient blob transfer retries once with a fresh temporary file; 429/503 responses allow one retry honoring Retry-After up to ten seconds; expired tags never silently fall back to stale data. Local reference/platform locks serialize local imports; remote HEAD publication uses conditional updates. `read`, `stat` and `list` require a published immutable handle and never pull implicitly.

Reference/architecture locks cover resolution/preparation; concurrent callers recheck/reuse the first successful result. Preparation may populate uncached images and retains existing digest extraction locks. `read`/`stat`/`list` require published immutable image handles and never pull images implicitly.

Directory pages include stat-equivalent attributes, avoiding one request per child. Pages contain at most 256 entries, shrinking to fit the JSON frame limit including long byte-array names/link targets. Directory attributes are required; name-only responses are rejected. Persisted pages retain attributes across mounts.

`kind` is `file`, `directory`, `symlink` or `special`. `mode` includes Unix type/permission bits; `target` holds link bytes or null. Prepared-source ownership/mode overrides are preserved; arbitrary xattrs and cross-server portable inode IDs are not provided. Only regular files are readable, and short reads including zero indicate EOF. Clients verify length and SHA-256 before caching. Hashes detect corruption without independently proving an untrusted server correct.

Error frame:

```json
{"status":"error","code":"not_found","message":"..."}
```

Codes are `not_found`, `permission_denied` and `request_failed` (including invalid arguments, unsupported versions and authentication failures). Framing errors may disconnect. Early close, truncated bodies and bad checksums are failures, never missing files or zero-filled content. Messages are explanatory, not machine-stable.

Server reads use the same bounded image, metadata-page and content-object caches as filesystem/S3 reads. OCI source inspection and its directory cache run during publication, rather than on guest file requests.

The server admits at most 64 connections and separates framing/idle waits from 16 file workers, with a 16-item file queue. Preparation has two separate workers and a 16-item queue. Saturation returns busy or closes unadmitted connections. Server idle timeout is five seconds; pooled clients expire at four seconds. Long preparations may continue after disconnect, and shutdown does not gracefully cancel individual OCI downloads. Automatic disk quotas and GC are not implemented.

There are no vCPU pause/resume messages. VM file requests run in the isolated runner; local filesystem and Unix sockets are accessed directly. Linux TCP/S3 uses a private Unix socket to a host access process, confined to the bound image's stat/list/read and whitelisted metadata, rejecting prepare, open and other images. Credentials remain in a hidden owner and the process ends with VM teardown. Host FUSE fetches content on the host.

## Backend validation

`mise run test -- pvisor pvisor-cli` includes server compatibility, independent filesystem/S3 CLI processes, read-only access, corruption refusal, and configuration precedence. The local S3 fixture independently verifies SigV4, including temporary session tokens, without real accounts, public requests, or an external daemon.

On Linux x86_64 with KVM and the static musl guest target, run the real VM acceptance check:

```sh
cargo nextest run --locked -p pvisor-cli --test cache_backends --run-ignored only
```

This starts VMs from both backends, validates guest file contents after deleting publisher staging, and verifies that inheriting the host environment does not leak storage credentials.
