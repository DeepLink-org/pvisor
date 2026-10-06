# pvisor-overlay-core

FUSE-neutral overlay mechanics and file-operation service shared by host FUSE
and VM virtio-fs, plus changeset review, apply, conflict detection and recovery.
Adapters own protocol inode identifiers, handles and permission translation.

## Validation

```sh
just test pvisor-overlay-core
```

## Request-local optimizations (R07–R09)

- **R07, safe subset:** fresh regular-file copy-up for `O_TRUNC` can omit the
  content copy only when the source has one link. Baseline preimage capture and
  journal ordering still precede upper publication. Existing uppers and shared
  hardlink inodes keep the normal path; adapters perform the actual open and
  truncation after inode/handle rebinding. This avoids the copy, not baseline
  hashing. Hash+copy fusion remains deferred: a descriptor-bound fingerprint
  callback must prove baseline/source identity and preserve first-observation
  publication races, composed lowers, remote backing and hardlink ordering.
- **R08, safe subset:** apply planning collects one request-local upper inventory
  containing relative raw paths, whiteout flags and no-follow metadata. Change
  classification and hardlink dependency closure share that inventory instead
  of walking/statting the upper independently. No inventory survives target
  publication or upper pruning. Publication-time target checks are unchanged;
  completion/recovery still collects fresh remaining changes after pruning.
  Eliminating those later walks would require a mutation-aware recovery inventory
  and is deferred rather than reusing stale metadata across mutation boundaries.
- **R09:** host FUSE uses the service's names/type directory candidates and loads
  attributes on demand for READDIRPLUS. Protected directory views still validate
  children at snapshot creation to hide denied hardlink aliases. See the host
  adapter README for cookie and inode lifetime details.
