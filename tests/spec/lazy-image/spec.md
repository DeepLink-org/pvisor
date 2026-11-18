# Lazy image publication through image push

Status: **draft, unreviewed feature request**
([#161](https://github.com/DeepLink-org/pvisor/issues/161)). Push, pull and the
daemon compatibility stage are implemented; final retirement remains pending.
Execution evidence and human approval
are separate; this feature does not change approval ledgers or snapshots.

## User workflow and boundary

Reuse the existing image catalog and command vocabulary. Build and ordinary
OCI pull already produce local V2 images. Publish those pinned local images
through a standard image operation with an explicit pVisor destination option:

```sh
pvisor build -t example:latest .
pvisor push example:latest --to s3://bucket/pvisor
# Equivalent grouped spelling:
pvisor image push example:latest --to s3://bucket/pvisor

pvisor image pull alpine:3.20
pvisor image push alpine:3.20 --to s3://bucket/pvisor

# On another machine; SOURCE is the immutable root returned by push.
pvisor image pull example:latest --from "$SOURCE"
pvisor image ls
pvisor image inspect example:latest
```

There is no separate conversion command and no upload flag on build in this
request. Retain `build --output type=oci,dest=PATH`, quiet output and iidfile
semantics. Do not add `cache pull/inspect/registry` or another mirror of image
management commands. Current `cache list/stat/read` inspect files *inside* an
image, while image ls/inspect inspect catalog objects; they are not aliases.
Keep file diagnostics as compatibility/internal tooling and do not promote
them as an additional user workflow or turn image ls into a directory listing.

`image push` provides explicit S3/file
publication mode. It does not claim that ordinary OCI-registry push is already
implemented: a push without `--to` must retain the OCI meaning or explicitly
refuse if unsupported, never infer an S3 target from ambient cache settings.
`pvisor push` and `pvisor image push` are equivalent entry points, following the
existing build/pull command grouping. They share argument parsing, local-source
selection, destination validation, publication, stdout/stderr and exit behavior.
All push contracts below apply to both spellings; help advertises both.

## Selection and output

| Operation | Contract |
| --- | --- |
| `image push LOCAL --to s3://BUCKET/PREFIX` | Publish the selected local image's fixed revision and dependencies |
| `image push LOCAL --to file:///DIRECTORY` | Produce the same self-contained bundle for static serving/offline staging |
| `image push NAME --images-dir DIR --to URI` | Select a prepared sandbox template in the existing daemon catalog explicitly |
| `image pull LOCAL --from SOURCE` | Import a fixed S3/HTTP(S) V2 publication into the ordinary image catalog |
| `image pull NAME --from SOURCE --images-dir DIR` | Import a prepared template into its existing daemon catalog |
| `image pull NAME --images-dir DIR --refresh` | Explicitly refresh a prepared name through the configured ordered V2 registries |
| `image ls/inspect ... [--images-dir DIR]` | Query the selected catalog without remote name resolution or content download |

`--images-dir` selects the existing prepared-template catalog rather than the
ordinary Docker store. Missing local sources are errors, never OCI pull or name
resolution. Snapshot the selected image revision once before upload; changing a
local tag during upload cannot change that publication. Use the image's recorded
platform; selecting another architecture belongs to build/pull, not conversion.

`--to` selects the destination from its URI. S3 and absolute file URIs are
writable; HTTP(S), relative file URIs, credentials, query strings and fragments
are rejected before publication. Ambient `PVISOR_CACHE_BACKEND/LOCATION/SERVER`
cannot redirect push. AWS credentials/region/endpoint remain host-side S3
configuration. Invalid arguments must not create a destination or launch work.

Successful explicit push writes one JSON object with `source`, `image_handle`,
`digest` and `platform`. `source` ends in the template/descriptor's 64-character
lowercase SHA-256 directory; the handle is the immutable `pvisor-v2:` identity.
`digest` is the source manifest identity and `platform` is `linux/ARCH`.
Diagnostics go to stderr. Repeated publication of the same pinned image and
portable metadata at the same target returns the same source.

Preserve the image inspect array and its existing Docker fields. A `Pvisor`
extension exposes `Source`, `ImageHandle`, `Digest` and `Platform` for a remotely
bound image; local-only images need no remote source. Querying images must not
silently pull. Do not overload image inspect with file stat/read side effects.

## Import and templates

Ordinary `image pull OCI` keeps full OCI acquisition and existing tag behavior.
Only explicit `--from SOURCE` selects lazy publication import. SOURCE is a fixed
S3/HTTP(S) bundle root, not an OCI registry or an incomplete shared-pool prefix.
Without `--from`, prepared-catalog resolution is selected only by explicit
`--images-dir`; ordinary Docker pulls never consult that registry configuration.

Import authenticates the descriptor/template and root metadata, then installs
the local reference atomically without fetching data objects. An existing name
requires `--refresh` in this explicit import mode; failed refresh leaves its
old record and pinned revision unchanged. Later reads stay on that publication
and cannot re-resolve a mutable tag to another version.

Ordinary descriptors use the existing `Image` record; prepared descriptors keep
`PreparedImage`. Catalog selection determines the descriptor contract. Bundles
also retain the original OCI config as `configs/SHA256.json` (at most 1 MiB),
authenticated by the image config ID before local retention. This preserves
labels, health checks, inspect fields and subsequent build defaults. Older
prepared publications without the asset remain importable for daemon runtime;
full OCI metadata queries then fail explicitly. Ordinary import requires it.

Prepared templates preserve engine/profile flags, entrypoint, cmd, environment,
E2B resource/version settings and the pinned V2 image. Rebind only host-local
paths. `pvisor-daemon prepare-image` and `registry` remain available until a
separate equivalent migration is specified; this request retires only `image`.
Existing daemon serve must consume records imported by the new image commands.
No arbitrary template DTO or new top-level template/cache manager is needed.

## Publication and failure invariants

Both ordinary images and prepared templates share the same immutable export,
integrity verification, object I/O and commit implementation. Push reads the
local V2 pool; it does not repull OCI or unpack a second full rootfs. It must
retain the existing reachable-pack export checks so removed private contents
cannot leak through unused pack slots.

Complete and verify content/index dependencies before making the descriptor
and name reference visible. Conditional-write conflicts fail instead of
overwriting an unobserved publisher; reconcile lost acknowledgements using
exact bytes/publication identity. Partial failures can leave unreferenced remote
objects but cannot replace an old visible publication or alter the source
catalog. Missing objects, corruption and authorization failures remain errors,
not empty bytes, nonexistent guest files or OCI fallback.

Keep AWS credentials out of URLs, portable JSON, logs and implicit guest
environment. Import/read clients write no remote objects. There is no bucket
creation, ACL/public-policy mutation, automatic GC, new format version, xattr
expansion or implied WAN performance claim.

Existing V2 shared-pool prefixes, immutable handles and self-contained S3/HTTP
publications remain readable. Existing `cache prepare/publish/list/stat/read`
remain legacy/diagnostic contracts while their user-facing workflow moves to
image commands. Their removal needs explicit migration of their consumers;
do not silently rewrite old shared-pool locations into bundle layouts.

## pvisor-daemon image retirement

| Legacy operation | Replacement |
| --- | --- |
| `image NAME --public --to URI` | `pvisor image push NAME --images-dir DIR --to URI` |
| `image NAME --from SOURCE` (S3 or HTTP(S)) | `pvisor image pull NAME --images-dir DIR --from SOURCE` |
| `image NAME --pull` | `pvisor image pull NAME --images-dir DIR --refresh` |
| Local `image NAME` query | `pvisor image inspect NAME --images-dir DIR` |

These rows refer to both `pvisor-daemon image` and `pvisor daemon image`.

1. **Transition:** prove new-entry parity, then announce one compatibility
   release cycle. Old image commands delegate to the shared implementation,
   preserve stdout JSON/failure semantics and warn on stderr with a replacement.
   Keep old implicit resolution only in the adapter; new inspect is a query.
2. **Final retirement:** ordinary daemon help no longer lists image. Explicit
   use fails with a `pvisor image` migration diagnostic before local/remote
   mutation, service startup or fallback execution. `prepare-image`, `registry`
   and service start/status/stop/serve are outside this removal.
3. Preserve legacy CID import and `--private` local IPFS retention during
   transition. Decide their replacement or removal before ending compatibility.
   Neither image rm nor a failed push revokes distributed bytes or deletes S3
   objects. This request does not add remote unpublish.

The retirement case remains XFAIL through transition. Maintainers choose the
release/version boundary; these draft tests do not authorize removing working
commands immediately.

## Executable evidence

| Case | Contract | Required result |
| --- | --- | --- |
| [S-LAZY-001](S-LAZY-001.md) | Ordinary build/image and service behavior, inert help/refusal | PASS |
| [S-LAZY-002](S-LAZY-002.md) | Image push/pull/query options, no duplicate cache tree | PASS |
| [S-LAZY-003](S-LAZY-003.md) | Equivalent push entry points, idempotence and explicit-target selection | PASS |
| [S-LAZY-004](S-LAZY-004.md) | Lazy HTTP import, exact data and atomic failure | PASS |
| [S-LAZY-005](S-LAZY-005.md) | Prepared native/E2B template publication and import | PASS; amd64 prerequisite |
| [S-LAZY-006](S-LAZY-006.md) | Final direct/forwarded daemon image retirement | XFAIL: image remains active |
| [S-LAZY-007](S-LAZY-007.md) | Legacy shared-pool readers, read-only behavior and corruption refusal | PASS |
| [S-LAZY-008](S-LAZY-008.md) | Destination/source admission and no implicit S3 push | PASS |
| [S-LAZY-009](S-LAZY-009.md) | Prepared adapters, registry resolution and legacy stdout/stderr parity | PASS |

Missing executables, OpenSSL or loopback service readiness fail even under
XFAIL. Strict mode rejects XFAIL; XPASS requires review of the declared gap.
Native fixtures use owned
resources and real CLI processes without a VM or public service.
S-LAZY-005 additionally requires an amd64 host because the existing daemon
prepare-image fixes its build platform to linux/amd64, including no-RUN builds.
An arm64 host must report that prerequisite failure rather than an accepted XFAIL.

Retain `pvisor-cli/tests/cache_backends.rs` and
`pvisor-daemon/tests/image_repository.rs` as the evidence owners for signed S3,
conditional writes, lost acknowledgements, bounded faults/retries, mirror trust,
atomic refresh and concurrent configuration. New entry tests supplement the
preserved legacy assertions. Their ignored TLS and
KVM guest/mmap/credential tests require explicit native runs.

Before final retirement, add explicit execd/envd inputs and concurrent
source-tag mutation against the new frontend.
These remain acceptance requirements, not claims established by the local
Scrut draft or fixture self-tests.
