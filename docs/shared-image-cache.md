# Shared OCI file cache, protocol v1

`pvisor cache serve` exposes the server's OCI image store as a read-only file
service. Clients never receive host filesystem paths. An image is prepared once
on the server, then addressed by its resolved platform manifest SHA-256 digest.
The existing store performs blob verification, layer application and whiteout
handling. File queries do not contact the registry.

## Usage

```sh
# Terminal 1: foreground server, default per-user Unix socket and OCI store
pvisor cache serve

# Terminal 2: use the same default socket
pvisor cache prepare alpine:latest
# Copy the digest from the JSON result:
pvisor cache list sha256:YOUR_MANIFEST_DIGEST
pvisor cache stat sha256:YOUR_MANIFEST_DIGEST etc/os-release
pvisor cache read sha256:YOUR_MANIFEST_DIGEST etc/os-release
```

`PERSISTING_PVISOR_CACHE_SERVER` selects the endpoint for both clients and server.
`cache serve --listen` overrides it on the server. With no override the endpoint
is `unix://<dirs::cache_dir()>/persisting/pvisor/cache.sock`:

- macOS: `~/Library/Caches/persisting/pvisor/cache.sock`
- Linux: `$XDG_CACHE_HOME/persisting/pvisor/cache.sock`, ordinarily
  `~/.cache/persisting/pvisor/cache.sock`

The server accepts `--image-store DIR` or `PERSISTING_PVISOR_IMAGE_STORE` for its
existing OCI store. It does not auto-start.

## Automatic VM lazy loading

When `pvisor --rootfs image=IMAGE -- COMMAND` prepares an OCI image for a VM,
it probes the default socket with a two-second `ping` handshake. A live compatible
server selects lazy loading automatically; a missing socket or refused connection
(stale socket) uses the existing local OCI preparation path. Authentication,
protocol and timeout errors are reported, not silently bypassed.

An explicit `PERSISTING_PVISOR_CACHE_SERVER` requires that service to work.
Set `PERSISTING_PVISOR_CACHE_SERVER=off` to force local preparation. Explicit
rootfs directories and native container execution retain their existing behavior.

The client mounts an immutable read-only FUSE lower (macFUSE FSKit on macOS;
FUSE on Linux), retaining the existing VM writable upper. Metadata is fetched on demand and retained in memory during the mount. With a
server that advertises `metadata_generation`, verified stat responses (including
missing paths) and directory pages are also persisted under
`<user-cache>/persisting/pvisor/metadata/v1/<endpoint-hash>/<manifest-digest>/<generation-hash>/`.
They survive VM exits; corrupt entries are fetched again. Older servers without
a generation keep the previous in-memory-only behavior. Generation includes the
server root directory identity and change time, so rebuilding the extracted root
invalidates metadata containing old host inode numbers. Prepared roots must stay
immutable; in-place edits below the root are not supported. Content is fetched in 1 MiB
blocks into `<user-cache>/persisting/pvisor/blocks/<endpoint-hash>/<manifest-digest>/`,
with per-file/per-block keys. On macOS, `<user-cache>` is `~/Library/Caches`;
on Linux it is `$XDG_CACHE_HOME`, ordinarily `~/.cache`. This block cache is
independent of `--image-store` and `PERSISTING_PVISOR_IMAGE_STORE`. Blocks are checksum-verified, published atomically and
shared across local processes using file locks. Cache hits are verified before
use; corrupted blocks are fetched again. No sparse placeholder files are exposed.
Normal kernel readahead may fetch adjacent bytes, and copy-up may read a whole
individual file. The client does not extract the full image.

The FUSE mount lives until the VM run finishes and is then unmounted; cached
blocks remain. Cached data can still be read after service failure, but missing
blocks fail with I/O errors. The digest and endpoint stay fixed for the run;
there is no mid-run registry fallback. The cache endpoint/token are removed from
implicitly inherited guest environment variables.

The server still fully prepares an uncached image before answering `prepare`.
This is client-side lazy loading, not lazy OCI layer extraction on the server.
The FUSE adapter and existing virtio-fs worker currently process requests
synchronously: a cache miss can delay unrelated filesystem requests. No explicit
vCPU pause is used. Cache quotas/eviction, original OCI xattrs and asynchronous
virtio-fs completions are not added by this implementation.

The public Rust client is `persisting_pvisor::cache::CacheClient::from_env()`.
It is blocking. `cache prepare/list/stat/read` continue to be explicit service
commands and do not use the VM's local fallback policy.

Unix sockets are mode 0600 and require the same effective user at both ends.
A lock prevents two servers from owning the socket; a stale socket is reclaimed
on restart, but a regular file, symlink or live listener is never removed.
Use a socket in a directory owned by the serving user. Stopping the foreground
server with Ctrl-C can leave a stale socket; no manual cleanup is needed.

For a remote server, use authenticated loopback TCP inside an SSH tunnel:

```sh
# On the server: set a strong shared secret through your secret manager/shell.
export PERSISTING_PVISOR_CACHE_TOKEN='YOUR_RANDOM_SECRET'
pvisor cache serve --listen tcp://127.0.0.1:7447

# On the client machine, keep this tunnel running:
ssh -N -L 7447:127.0.0.1:7447 your-server

# Client shell, using the same secret:
export PERSISTING_PVISOR_CACHE_TOKEN='YOUR_RANDOM_SECRET'
export PERSISTING_PVISOR_CACHE_SERVER=tcp://127.0.0.1:7447
pvisor cache prepare alpine:latest
```

TCP requires a nonempty token and accepts only literal loopback IP endpoints.
There is no built-in TLS; use SSH for transport encryption. A token grants all
cache operations, including preparation of new images, so this is a trusted
shared service, not a public multi-tenant API. If a token is configured for a
Unix server, Unix clients must supply it too.

## TUI download statistics

With `--tui`, lazy image runs show three groups, each with file count and size:
`Cached` (local cache reads), `Transferred` (content received from the image
server, whether local or remote), and `Total` (the whole image). Narrow status
bars abbreviate them to `C / X / T`; Overview shows all three rows with exact
byte counts. The Log panel records every
verified block transfer with its file path, byte count and cumulative run totals.
The status bar and Overview also show local cache reads: distinct file paths
and cumulative verified block bytes, including repeated reads. These are separate
from downloads; they exclude reads satisfied by the host or guest kernel page
cache. Each file's first local cache hit appears in Log as `no download`.
Directory listings only fetch metadata and do not count as content reads. Image startup and I/O diagnostics
also go to this panel instead of the guest terminal; without TUI they use stderr. Downloads count this run's
verified content received from the cache server, excluding local cache hits,
protocol metadata and guest network traffic. A partially downloaded file counts
once; repeated transfers add bytes again. A warm run can therefore show zero
downloads even while using the image.

Totals count regular file paths and their uncompressed logical sizes in the
server's extracted image, including empty files and each hard-link name.
Directories and symlinks are excluded. These totals are cached per manifest
digest after a metadata-only scan. They describe the whole image, not compressed
OCI layer sizes. Older servers without totals display `?`.

## Wire format

Each connection carries exactly one request and one response, then closes.
A frame is a four-byte unsigned big-endian JSON byte length followed by UTF-8
JSON. JSON frames are limited to 1 MiB. A successful `read` response frame is
followed immediately by exactly `length` raw bytes. Other responses have no
binary body. There are no unsolicited messages or compression.

Request envelope:

```json
{"version":1,"token":null,"request":{"op":"read","digest":"sha256:...","path":[101,116,99,47,111,115,45,114,101,108,101,97,115,101],"offset":0,"length":1048576}}
```

Paths and directory names are JSON arrays of Unix filename bytes, preserving
non-UTF-8 filenames. Paths are relative to the image root; an empty path means
the root directory. Absolute paths, parent traversal and NUL are rejected.
Symlinks are returned as metadata, never followed by server path resolution.
A future filesystem client must resolve guest symlinks within the guest tree.

| `op` | Fields | Response `status` |
| --- | --- | --- |
| `ping` | none | `ready` (protocol v1) |
| `prepare` | `image`, `architecture` (`amd64` or `arm64`) | `prepared`: `digest`, `architecture`, `env`, `entrypoint`, `cmd`, optional `totals` (`files`, `bytes`), optional `metadata_generation` |
| `list` | `digest`, `path`, `offset` (entry index, start at 0) | `entries`: sorted `names` (up to 256), `next_offset` (null when done) |
| `stat` | `digest`, `path` | `metadata`: `kind`, `size`, `mode`, `uid`, `gid`, `inode`, `nlink`, `mtime`, `mtime_nsec`, `target` |
| `read` | `digest`, `path`, `offset` (byte offset), `length` (1..1048576) | `data`: `length`, `sha256`, followed by raw bytes |

`prepare` requests Linux images for the client's architecture, independent of
the server architecture. It resolves mutable tags on each call; the returned
platform manifest digest pins subsequent requests. Preparation can populate an
uncached image and uses existing per-image extraction locks. It may still need
the registry even when an extracted root already exists. `read`, `stat` and
`list` require an already prepared digest; they never implicitly pull an image.

`kind` is `file`, `directory`, `symlink` or `special`. `mode` includes Unix type
and permission bits. `target` contains symlink bytes or null. Attributes reflect
the extracted server filesystem; v1 does not reconstruct original tar ownership,
provide xattrs, or define portable inode IDs across servers. Only regular files
can be read. A short read, including zero bytes, indicates EOF. Clients must
check the body length and SHA-256 before admitting bytes to a cache. The supplied
hash detects transfer corruption; it is not an independent proof against an
untrusted server. The server and its local image store are trusted.

Errors are frames such as:

```json
{"status":"error","code":"not_found","message":"..."}
```

Codes are `not_found`, `permission_denied`, and `request_failed` (including
invalid arguments, unsupported protocol versions and failed authentication).
Malformed framing may also cause disconnection. Clients must treat an early
close, truncated body or bad checksum as failure, never as a missing file or
zero-filled content. Error messages are explanatory, not machine-stable.

The server shares in-memory caches for up to 4096 stat responses and 128 sorted
directory indexes across requests. Directory pagination reuses the same index
instead of rescanning and sorting on every page. Each cache clears at its entry
ceiling and is rebuilt lazily after a server restart. These caches cover metadata,
not mutable tag resolution; `prepare` still contacts the registry.

The server has 16 workers and at most 16 queued connections. Excess connections
are closed; clients may retry. Socket reads/writes have a 300-second inactivity
timeout, and TCP connects have a 10-second timeout. Long-running preparation may
outlive a disconnected client; retrying is safe. Shutdown does not cancel
individual OCI downloads gracefully. Registry download limits and cache eviction
remain those of the existing image store; v1 adds neither quotas nor eviction.

No vCPU pause/resume messages exist. Downloads happen in the host-side FUSE
server, outside the sandboxed VM runner process.
