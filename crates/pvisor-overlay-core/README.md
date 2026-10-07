# pvisor-overlay-core

FUSE-neutral overlay mechanics and file-operation service shared by host FUSE
and VM virtio-fs, plus changeset review, apply, conflict detection and recovery.
Adapters own protocol inode identifiers, handles and permission translation.

## Validation

```sh
just test pvisor-overlay-core
```

## Copy-up and truncation

Fresh regular-file copy-up for `O_TRUNC` can omit the content copy only when the
source has one link. Baseline preimage capture and journal ordering precede
upper publication. Existing uppers and shared hardlink inodes keep the normal
path; adapters perform the actual open and truncation after inode/handle
rebinding. This avoids the copy, not baseline hashing. Baseline capture and
content copying are separate: any descriptor-bound fingerprint callback must
prove baseline/source identity and preserve first-observation publication
races, composed lowers, remote backing and hardlink ordering.

## Apply inventory and publication

Apply planning collects one request-local upper inventory containing relative
raw paths, whiteout flags and no-follow metadata. Change classification and
hardlink dependency closure share that inventory. No inventory survives target
publication or upper pruning. Target checks occur at publication time;
completion/recovery collects fresh remaining changes after pruning. Recovery
must not reuse stale metadata across mutation boundaries.

Entry publication exclusively creates a private random temporary directory;
reserved-looking host names are never cleanup authority. Interrupted copies can
leave these directories behind, and retries do not sweep them. Replacement
backups require a durable ownership receipt before reuse or cleanup. Legacy
backups without that receipt are retained and require manual inspection, not
automatic adoption. Terminal `overlay.json` publications are authoritative over
an older matching Run overlay identity/generation. While the apply ledger is
pending, runtime reads project the terminal state to block drop but retain the
original generation for recovery. Apply reconciles under the Job mutation/Run
lease and target lock, then publishes the new runtime fence only after ledger
commit. Recovery failures are errors, not `AlreadyApplied`. This does not make
`run.json` and the core ledger an atomic publication.

## Directory enumeration

Host FUSE uses the service's names/type directory candidates and loads
attributes on demand for READDIRPLUS. Protected directory views validate
children at snapshot creation to hide denied hardlink aliases. See the host
adapter README for cookie and inode lifetime details.
