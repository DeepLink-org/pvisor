# Shared image cache and storage backends

Use `pvisor push` to distribute a local image, and use the Rust cache client to inspect immutable image contents. `pvisor-daemon serve --cache` prepares OCI images for VM clients and stores their indexes and content in the local Image Pack pool. File queries select a pinned `image_handle` and do not contact an OCI registry.

## Local image directory {#local-image-store}

Docker-compatible commands, spawn without a selected remote backend, and the cache server share one local image directory. Its default is `~/Library/Application Support/pvisor/docker/images/` on macOS and `$XDG_DATA_HOME/pvisor/docker/images/` on Linux, usually `~/.local/share/pvisor/docker/images/`. Revisions referenced by tasks belong in persistent user data storage.

An explicit Session/cache image-store option takes precedence, followed by `PVISOR_IMAGE_STORE`. With only `PVISOR_DOCKER_HOME` set, all local entry points use its `images/`, preserving isolated storage. Embedded `Store::open(root)` always uses `root/images`, independently of environment defaults. Container state, Session records and writable layers keep their separate owners. The local image root and its control directories must be owned by the current user, private (0700) and not symlinks; incompatible existing directories fail without changing their permissions.

```text
<image-store>/
├── configs/<config-digest>.json
├── image-pack-v1/
│   ├── format.json
│   ├── meta/<image-key>/...
│   └── data/blobs/...
├── staging/.leased-import-v1-*/
├── locks/
└── projections/
```

Download and extraction directories under `staging` are normally removed when the import ends.
After a process crash, `pvisor image prune-staging` removes abandoned staging directories with import leases. Active imports, staging directories from older releases and published images are retained. `configs` retains digest-checked original OCI configuration. New local images use `image-pack-v1`; pinned prerelease V2 images retain their old locations. Docker pull still resolves the registry, but reuses matching shared-pool digests. Build commits each tag independently. `rmi` removes catalog references without reclaiming shared content; active and stopped tasks retain fixed revisions.

If a lease cannot be read or is invalid, the command preserves that directory and keeps checking the others. It exits with an error that reports the number removed and the first affected path.

Short and fully qualified Docker Hub aliases share one canonical catalog identity, including pull/build replacement, create/FROM lookup and rmi. Equivalent build tags publish once. Distinct tags and existing containers retain their pinned revisions. Catalogs containing duplicate canonical identities fail explicitly; rebuild those catalogs in a fresh store.

Local pool tags stay available offline until an explicit pull/refresh or another publication replaces them. The five-minute automatic tag refresh applies to writable filesystem/S3 backends, not the local pool.

Image records bind the selected platform and the content-pool revision.

## Choose a backend

Filesystem, S3, Google Cloud Storage and cache servers write [Image Pack V1](../design/experimental/image-pack/index.md). New pools default to a 32 MiB blob target; to initialize a separate pool at 64 MiB, set `PVISOR_IMAGE_BLOB_TARGET_MIB=64` on the writer. Existing pools keep their initialized policy.

For `PVISOR_CACHE_LOCATION`, the URL scheme selects the backend; `PVISOR_CACHE_BACKEND` is unnecessary. Direct storage backends do not require `pvisor-daemon serve --cache`. Without a repository URL, existing cache server discovery applies.

| URL | Access | Use case |
|---|---|---|
| `file:///ABSOLUTE/DIRECTORY` | Read and write | Local or shared filesystem repository |
| `s3://BUCKET/PREFIX` | Read and write | AWS S3 or an S3-compatible service |
| `gs://BUCKET/PREFIX` | Read and write | Google Cloud Storage through Apache OpenDAL |
| `https://HOST/PREFIX` or `http://HOST/PREFIX` | Read only | Static mirror with exact Range GET |

`PVISOR_CACHE_SERVER=unix://...` or loopback `tcp://...` selects a pVisor cache service, not a repository. The legacy `PVISOR_CACHE_BACKEND=filesystem` with a bare absolute path still works; prefer a `file:///` URL for new configurations.

Set `PVISOR_CACHE_READ_ONLY=true` to make an embedded client read-only. A read-only client cannot refresh or populate a missing shared-pool name, and may still keep a private acceleration cache.

### Publish a local image {#publish-a-local-image}

On the publisher, configure AWS credentials and region and use an existing bucket. Copy the receipt's `source` to the receiving machine. The example uses `jq` to extract that value.

```sh
pvisor pull alpine:3.20
SOURCE=$(pvisor push alpine:3.20 --to s3://your-bucket/releases | jq -er .source)
pvisor image pull alpine:published --from "$SOURCE"
```

Use `--to file:///ABSOLUTE/PATH` to write an offline bundle. On a host that can access the directory, pass the returned `file:///` URL directly to `image pull --from`; no HTTP server is needed. The same bundle can be served by a static HTTP(S) mirror. Import reads the descriptor, OCI configuration and root metadata; file contents are read on demand. After importing a `file:///` publication, keep the directory available and immutable.

S3 uses SigV4 over HTTPS. Publishers need `s3:GetObject` and `s3:PutObject`; readers need `s3:GetObject`. Host environment credentials or workload roles supply authentication. For an S3-compatible service, set `AWS_ENDPOINT` and the region.

Use `--to gs://BUCKET/PREFIX` to publish to an existing GCS bucket. OpenDAL uses host Google Application Default Credentials; set `GOOGLE_APPLICATION_CREDENTIALS` when using an explicit credential file. GCS conditions creates and updates on object generations. Keep credentials out of URLs, image descriptors and receipts.

### Shared-pool layout

An existing shared-pool prefix remains usable by VM cache clients and the Rust SDK. Its mutable HEAD records serve shared-pool name resolution. A push produces an independent bundle with an `image.json` descriptor, so import it by its returned source.

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

The writer groups small-file frames bottom-up by directory and packs large-file frames in file-offset order. Indexes point to compressed byte ranges and slots. Only complete encoded blobs deduplicate across revisions; previous HEAD does not affect placement. See the [packing rules](../design/experimental/image-pack/index.md#packing).

### Layout and request costs

Individual binary metadata objects are capped at 64 MiB and control objects at 1 MiB. Limits are 1,000,000 entries and 2,000,000 content spans; the object-size cap can be reached first. S3-compatible services must support Range GET, If-None-Match creation and If-Match updates.

Publishing writes and verifies shared content, per-image metadata, and COMMIT before CAS-updating its own platform HEAD. Only one publisher can commit against a given old HEAD; conflicts fail explicitly rather than blindly overwriting. Corrupt existing immutable objects cannot be reused or overwritten while other images may reference them. Failures can leave unreferenced revisions/objects. Upload completion records remain in uploads for offline maintenance.

Open fetches controls, root.bin and the root-directory frame without parsing the full file list. Lookup/readdir load independent compressed index frames; V1 content reads fetch only the required compressed frame ranges. The index LRU has a 16 MiB decoded residency limit and content a 64 MiB/4,096-entry limit. Non-pool readers may persist encoded frames under <user-cache>/pvisor/cache-v2/objects/<location-hash>/. Ordinary local-pool reads create no persistent raw image mirror. Corrupt local caches are refetched; remote absence, corruption and authentication failures fail explicitly, without zero filling or registry fallback.

Writable mode reuses mutable-tag HEADs for five minutes; publishers update with --refresh. Read-only mode always uses published HEADs without registry queries. Tasks pin returned image_handles, and older revisions remain readable while retained. IMAGE@sha256:… is an independent reference whose meta must be published separately; a tag's returned image_handle already pins that version.

Actual S3 traffic includes control objects, metadata pages, and complete data chunks. Logical file bytes returned by the cache API are not billed traffic. Automatic GC/quotas are not implemented. Disable relevant sessions/publications and confirm readers exited before deleting meta/data; never delete shared objects based solely on age.

## Code layout

Implementation lives in `crates/pvisor/src/image/cache/`:

`pvisor-daemon serve --cache` owns the server entry point. The runtime exports the Rust client.

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

Start the cache server, then send a Prepare request through the Rust client. The source example `cache_probe` takes a request as JSON, or reads a whole file by handle and path. Its process uses `CacheClient::from_env()`. Use `Request::List`, `Request::Stat` and `Request::Read` for file-level diagnosis in your own tools.

```sh
pvisor-daemon serve --cache

# From a checkout, in a separate terminal on an amd64 host:
cargo run --locked -p pvisor --example cache_probe -- \
  '{"op":"prepare","image":"alpine:3.20","architecture":"amd64"}'
```

`PVISOR_CACHE_SERVER` selects the endpoint for client/server; `pvisor-daemon serve --cache --cache-listen` overrides it on the server. The default is `unix://<dirs::cache_dir()>/pvisor/cache.sock`:

- macOS: `~/Library/Caches/pvisor/cache.sock`
- Linux: `$XDG_CACHE_HOME/pvisor/cache.sock`, usually `~/.cache/pvisor/cache.sock`

Use `--cache-image-store DIR` or `PVISOR_IMAGE_STORE` to select the server’s local image directory, shared with Docker and daemonless VM Sessions. The server does not start automatically.

## Automatic VM lazy loading

When no filesystem/S3 backend is selected and `pvisor spawn --executor vm --rootfs image=IMAGE -- COMMAND` prepares an OCI image, it probes the default socket with a two-second `ping` handshake. A compatible server enables lazy loading automatically. Missing/refused sockets (including stale sockets) prepare and read through the same local V2 pool. Authentication, protocol and timeout errors are reported rather than bypassed silently.

An explicit `PVISOR_CACHE_SERVER` requires the service. `PVISOR_CACHE_SERVER=off` forces the local V2 pool. Explicit directory rootfs and native containers retain their behavior.

VM clients attach an immutable read-only lower directly to the virtio-fs service, retaining their writable upper without an intermediate host FUSE mount. Host tools retain the FUSE adapter over the same backend. Metadata is fetched on demand and cached in memory for the backend lifetime. `metadata_generation` is the revision COMMIT digest. For socket/filesystem/S3 clients, verified attributes and directory pages can persist across runs; rebuilding OCI extraction does not change a published revision.

Socket/filesystem/S3 clients cache content in 1 MiB blocks at `<user-cache>/pvisor/blocks/<endpoint-hash>/<read-handle-hash>/`, keyed by file/block. `<user-cache>` is `~/Library/Caches` on macOS and `$XDG_CACHE_HOME` (usually `~/.cache`) on Linux. This is independent of `--image-store`/`PVISOR_IMAGE_STORE`. Small files use one unpadded block; large files fetch only accessed blocks.

Each backend retains validated content in a per-file memory cache capped at 64 MiB/4096 blocks with FIFO eviction. Hot reads copy the requested slice without reopening/rehashing disk blocks. Memory misses revalidate disk content; disk corruption cannot alter already validated in-memory bytes. Validated new blocks publish atomically and use file locks for local process sharing; corrupt blocks are fetched again. VMs maintain a private metadata projection with sparse placeholders, but guest READ uses backend content instead of placeholder holes. Kernel readahead may fetch adjacent bytes; copy-up and file digests require complete originals, and complete checkpoints or self-contained exports populate the full tree. Ordinary lazy reads do not extract the complete image.

Direct backends stay attached until VM completion, then release their private projections; host FUSE mounts remain until their users exit. Cached blocks remain. Cached content survives service failure, but missing blocks return I/O errors. Digest/endpoint stay fixed for a Run with no mid-run registry fallback. Cache endpoint/token are removed from implicitly inherited guest environment.

Before responding to `prepare`, the server fully prepares an uncached image and generates indexes/content objects in a temporary import directory, removed after success or an ordinary error. This is client-side lazy loading, not lazy OCI layer extraction on the server. FUSE adapters and existing virtio-fs workers handle requests synchronously, so a miss can delay unrelated filesystem requests. There is no explicit vCPU pause. Disk quotas/eviction and asynchronous virtio-fs completion are outside this implementation; OCI layers containing xattrs or device nodes are rejected at import rather than silently losing metadata.

The blocking Rust client uses its selected backend directly. Automatic local fallback belongs to VM source selection.

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
cargo run --locked -p pvisor --example cache_probe -- \
  '{"op":"prepare","image":"alpine:3.20","architecture":"amd64"}'
```

TCP requires a token of at least 16 bytes and literal loopback IP endpoints. There is no built-in TLS; use SSH encryption. Tokens grant all cache operations, including preparing new images. This is a trusted shared service rather than a public multitenant API. Unix clients must also supply tokens when configured on the server.

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

`prepare` requests a Linux image for the client's architecture, independent of server architecture. The published V2 HEAD contains the platform digest and launch configuration; no persistent raw prepared-image record is required. Local pool tags remain available offline until explicit refresh or replacement. Writable filesystem/S3 backends reuse mutable HEAD for five minutes and immutable digest HEAD until refresh. Set `refresh: true` to force registry resolution. Failed imports preserve the previous HEAD and remove their temporary directories. Registry connections have a 10-second connect timeout; blocking requests and body reads have a 30-second timeout. A transient blob transfer retries once with a fresh temporary file; 429/503 responses allow one retry honoring Retry-After up to ten seconds; expired tags never silently fall back to stale data. Local reference/platform locks serialize local imports; remote HEAD publication uses conditional updates. `read`, `stat` and `list` require a published immutable handle and never pull implicitly.

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

`mise run test -- pvisor pvisor-cli` includes server compatibility, filesystem/S3 access from independent Rust cache clients, read-only access, corruption refusal, and configuration precedence. The local S3 fixture independently verifies SigV4, including temporary session tokens, without real accounts, public requests, or an external daemon.

On Linux x86_64 with KVM and the static musl guest target, run the real VM acceptance check:

```sh
cargo nextest run --locked -p pvisor-cli --test cache_backends --run-ignored only \
  -E 'test(vm_reads_daemonless_filesystem_and_s3_images_without_projecting_storage_credentials)'
```

This starts VMs from both backends, validates guest file contents after deleting publisher staging, and verifies that inheriting the host environment does not leak storage credentials.
