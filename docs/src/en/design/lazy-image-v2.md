# Lazy image V2: compressed directory indexes and content pools

V2 is the shared storage format for server, filesystem, S3 and Docker local pools. All filesystem metadata lives in a lazily loaded index; small files are packed by directory, and data objects contain only content and placement tables. Implementation: `crates/pvisor/src/image/cache/portable/`.

| Item | Current contract |
| --- | --- |
| Storage version | `format_version=2`, `pvisor-radix-v2`, `pvisor-v2:` handles |
| Routing | XXH3-64 with seed `0x707669736f727632` of raw basename bytes; 4 bits per level |
| Integrity | SHA-256; the fast routing hash provides no integrity guarantee |
| Compression | Independent Zstd level 3 frames with the standard seekable footer |
| Migration | V1 reader and publisher removed; republish OCI sources into an empty directory/prefix |
| Performance evidence | [Native Docker comparison](../lab-notes/image-format-performance-analysis.md); storage, startup and memory measured separately |

## Metadata and directory boundaries {#architecture}

Each directory owns only its immediate children. An ordinary directory uses one compressed leaf frame. When a leaf's uncompressed encoding exceeds 64 KiB, the next hexadecimal digit of the name hash splits it into up to 16 children, recursively if needed. Directories define locality, not an indivisible minimum frame: huge directories still split. Bounded raw-name range nodes handle full 64-bit hash collisions; exact name comparison determines existence.

Leaf records inline the name, type, size, mode, uid/gid, inode/nlink, mtime/nsec, symlink target, content SHA-256 and content reference. Subdirectory records point to their own index roots. Paths, names and symlink targets retain raw bytes without requiring UTF-8. There are no wildcards, probabilistic existence answers or global per-file placement tables. Inode and content identities remain separate; equal content does not merge distinct file inodes.

Index and data-frame payloads use pinned bincode 2.0.1 standard encoding (little-endian, variable-length integers); controls remain JSON. Outer format markers and handle versions select the decoder. Changing a version field cannot convert old objects.

Branches record each child frame's position, compressed/uncompressed lengths, encoded-byte SHA-256 and entry count. Parents logically contain subtrees without recompressing already compressed children. All frames are written child-before-parent into one `index.bin`; references encode the hierarchy. Readers use direct Range GETs and do not download the global seekable footer for a lookup. The small `root.bin` holds the root record authenticated by COMMIT, anchoring the authenticated tree and replacing V1's global page-checksum catalog.

Directory enumeration sorts by `(XXH3(name), name_bytes)`, with at most 256 entries per page. Continuations are directory-bound opaque cookies, never file indexes. Each page returns complete attributes. Authenticated subtree counts skip unrelated nodes instead of downloading the entire directory for every page.

## Content pools and small-file placement {#content}

| Parameter | Current value/meaning |
| --- | --- |
| Small-file threshold | Regular files ≤64 KiB; empty files have no content object |
| Pack target ceiling | 1 MiB deduplicated raw payload, at most 4,096 items |
| Data frame | Target 16 KiB raw payload, at most 1,024 slots; one file ≤64 KiB stays intact |
| Large-file chunk | At most 1 MiB of independent raw content |
| Compression unit | One frame; every object has the standard seekable footer |

Publication groups files by directory and packs content in SHA-256 order within each group. Metadata hash buckets and content packing are independent, preserving directory content locality. Equal small-file content has one placement within a revision; cross-directory duplicates belong to the first directory in deterministic traversal. Placements in the previous HEAD are reused by content identity, so rename, cross-directory moves and chmod can produce only a new index. Different publication histories need not produce identical packs, and partially overlapping small-file sets across images do not guarantee physical per-file deduplication.

An indexed small-file reference is `blob + frame_id + slot_id`, without a per-file byte offset. A blob's layout frame holds data-frame references; slot tables inside data frames hold file offsets. Reads authenticate the layout and target frame, select the slot and verify the file SHA-256. These tables describe content placement and contain no paths, permissions, timestamps or other filesystem metadata.

Small-file objects use `data/packs/<aa>/<bb>/<encoded-blob-sha256>`. Large-file chunks use `data/sha256/<aa>/<bb>/<plaintext-chunk-sha256>`, also with the new framed encoding; identical large chunks can be shared across paths and images. Changing one file may add a new pack. Old packs remain while old revisions reference them; online GC is not implemented.

## Publication, opening and failure semantics {#publication}

```text
format.json
meta/<image-key>/identity.json
meta/<image-key>/platforms/<platform>/HEAD.json
meta/<image-key>/platforms/<platform>/revisions/<revision>/
  manifest.json  config.json  root.bin  index.bin  objects.bin  COMMIT.json
meta/<image-key>/platforms/<platform>/uploads/<upload-id>/
  plan.json  progress.json
data/packs/<aa>/<bb>/<encoded-blob-sha256>
data/sha256/<aa>/<bb>/<plaintext-chunk-sha256>
```

An open pins image-key, platform and revision. Publication writes content dependencies, indexes/configuration, COMMIT, then CAS-updates HEAD, the visibility commit point. Existing immutable objects are verified before reuse, never overwritten if corrupt. HEAD conflicts fail explicitly, failed publication can leave unreferenced objects, and lost ACKs are reconciled through the existing transaction rules. `objects.bin` is an offline dependency inventory, not a global table required at open.

Open reads only controls, the root descriptor and root-directory frame. Path lookup loads relevant frames at each level; content Range reads and decompression hold no service-wide lock. Same-key cold misses coalesce, with at most 128 different concurrent misses and explicit failure on saturation. Network, authentication and integrity failures never become ENOENT. Encoded remote bytes are verified before bounded decompression with input, window and output limits. Corrupt local encoded caches are refetched.

| Resource | Limit |
| --- | --- |
| Entries/content spans per image | 1,000,000/2,000,000 |
| Individual metadata/control object | 64 MiB/1 MiB |
| Decoded frame | Approximately 1 MiB including large-chunk encoding overhead; ordinary index leaves target 64 KiB |
| Index LRU per loaded image | 16 MiB decoded residency budget/4,096 entries |
| PortableCache content LRU | 64 MiB/4,096 entries |
| Loaded images per PortableCache | 4 |

Charges survive eviction until the last active reference is released. One `index.bin` still has a 64 MiB limit: long names/attributes can cause rejection before the entry limit. Publication metadata planning still requires O(N) memory; encoded content is staged to disk and eight upload workers are bounded. External sorting and unbounded index capacity are not implemented.

## Filesystem integration and local storage {#filesystem-access}

Docker pulls/builds, daemonless VM Jobs and cache services use the same `<image-store>/pool-v2`; see the [local image directory](../reference/shared-image-cache.md#local-image-store) for defaults and overrides. Local pools have a `pool-format.json` marker but share the filesystem/S3 compressed physical format, without a separate PVPOOL envelope. Ordinary pool reads retain bounded decoded memory rather than a persistent complete extracted image. Non-pool direct readers may persist encoded frames under `<user-cache>/pvisor/cache-v2/objects/`; disk caches have no aggregate eviction quota.

VM direct lowers and host FUSE share the reader. VMs use private metadata/sparse-file projections; ordinary guest READs use backend content, while copy-up, complete exports and checksums may read whole files. Guest mmap still uses virtio-fs and the guest page cache, with no new DAX path. Existing verified file-block caches in remote VM adapters may still persist raw blocks and must be distinguished from local pools. First preparation still downloads and applies OCI layers; this format does not provide lazy registry-layer extraction.

Arbitrary xattrs, ACLs and security labels are not represented. Local pool imports reject unrepresentable xattrs; direct publication also provides no xattr/ACL fidelity contract. Existing ownership/mode overrides remain supported; macOS local pools ignore only the host provenance attribute. Protocols 1/2, connection pools and reader-prefetch switches belong to transport and are independent of storage V1 retirement.

V1 handles, formats and pool markers fail explicitly, without in-place rewriting or automatic deletion. A new pull can rebuild V2 from OCI; containers pinned to old handles need recreation. Old directory-based instances keep their existing path behavior. RAFS is not binary-compatible with this format, and standard seekable framing does not make it directly mountable as a Nydus image.

## Cache boundaries and cost model {#cache-costs}

Compressed storage and remote on-demand delivery share a format but incur different costs. A Docker-style local pool already holds all encoded content after pull. A remote client can read files on demand only after the image service has prepared them. Initial OCI download, extraction and publication move to the service and can be amortized across clients; they do not disappear.

| Path | Persistent storage | First read | Subsequent reads |
| --- | --- | --- | --- |
| Local Docker-style pool | Complete compressed indexes/content, without a complete extracted tree | Load directory frames, authenticate and decompress content frames | Bounded decoded LRU and guest page cache; a new instance still boots a VM |
| Direct filesystem/S3 | Complete publisher-side encoded pool; clients may cache fetched encoded frames | Controls and relevant index/content ranges | Encoded cache and decoded LRU; no aggregate disk-cache eviction quota |
| Cache server/remote VM | Service-side encoded pool and potentially retained OCI staging; client indexes and verified raw file blocks | Metadata RPCs and accessed file content, potentially decoded for transport | A new VM can reuse client blocks; each guest also has its own page cache |

Local disk savings therefore do not equal remote transfer savings. Raw Read data from the file service can exceed compressed OCI layers. Full scans, verification, exports and copy-up enlarge the working set. Small-file packing reduces stored object counts without guaranteeing a matching reduction in file RPCs. Inline metadata returns attributes with a hit, but cold lookups still resolve parent paths and relevant leaf frames.

Capacity savings mainly avoid resident extracted copies. Indexes, placement tables and pack layouts add overhead, so the encoded pool need not be smaller than original compressed OCI blobs. Individual mechanisms need separate experiments; a whole-inventory difference cannot assign benefits to each optimization.

`mmap` does not directly map compressed blobs. Guest faults fill the page cache through virtio-fs READs, with backend authentication/decompression on that path; warm pages can hide cold-miss cost. Sequential scans, random first touches, repeated warm reads and writable copy-up are distinct workloads. One startup probe cannot establish mmap performance for all of them. Decoded-cache budgets are not whole-VM, service or host memory limits.

## Benefit validation and scope {#validation}

The [lazy-image benchmark](../benchmarks/lazy-image-startup.md) owns user decisions: complete local inventories, prepared instances, remote cold/warm clients and large-working-set read costs are measured separately against same-digest native Docker. Capacity includes metadata and normal content sharing on both sides; service preparation and client waiting have separate clocks. Cold clients have empty private caches, not cold host page caches. Loopback results do not establish WAN performance.

The [native Docker engineering record](../lab-notes/image-format-performance-analysis.md) retains full launch/exit and whole-group memory accounting. Separate guest kernels, container memory limits and guest RAM have different scopes. Lower image disk usage does not establish a universal startup, per-instance memory or throughput advantage. Storage V1 is no longer the main benefit comparator, and historical transport-optimization switches must not be confused with storage-format versions.

Real S3/WAN, concurrent startup, random mmap working sets, writes/copy-up, import peaks and long-term cache reclamation still require independent validation. O(N) publisher planning, the individual index-object limit and missing online GC remain scaling boundaries. Static compression ratios establish neither unlimited image capacity nor stable long-term disk use.
