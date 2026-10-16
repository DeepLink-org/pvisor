# pVisor application frontends

`pvisor-cli` owns four executables: `pvisor`, `pvisor-cache`, `pvisor-tui`, and
`pvisor-replay`, with their argument parsing, terminal adapters, rendering and
companion lookup.
The TUI frontend lives here, not in the embeddable runtime.

## Boundary

Embed [`pvisor`](../pvisor/README.md) for `PVisor`, Session/Attempt execution and
`job_service::RuntimeJobService`. This application library exposes frontend
modules, not a compatibility re-export of the runtime. Frontends call explicit
runtime APIs and retain request-local cancellation, admission fences and terminal
ownership. The persistent Host Job listener is not an implicit requirement of
embedded execution.

The separate `pvisor-daemon` owns shared services, including the memory pool.
Invoke `pvisor-daemon` and `pvisor-cache` directly. Replay and TUI are root
companions. Use `pvisor -- COMMAND` for explicit workload execution.

Recording callers import `Journal` and `JournalStore` from `pvisor_journal::api`;
durable stage preparation uses its `Persistence` and `DurableFiles` contracts.

## Command help and live VM controls

Root help stays concise: **Execution** (`run`, `status`, `kill`), **Changes**
(`review`, `apply`, `drop`, `inspect`), **Checkpoints** (`checkpoint`, `suspend`,
`resume`, `fork`) and **Tools** (installed `replay`/`tui` companions, `feature`,
`help`). It includes the run-review-apply examples; use `pvisor help COMMAND`
for command options.

```bash
pvisor run --safe -- claude
pvisor review last
pvisor apply last --path src
```

The seven live VM flags are command-local, under **Live VM** in command help:

| Command | Accepted live VM flags |
| --- | --- |
| `status` | `--vm-socket`, `--vm-job-id`, `--vm-attempt-id` |
| `suspend` | The three identity flags plus `--vm-pause`, `--vm-offload`, `--vm-ram-file` |
| `resume` | The three identity flags plus `--vm-load` |

Place these flags after the supported command. Root-prefixed live VM flags and
live VM flags on other commands are rejected. All three identity flags are
required together; suspend/resume's positional Job must match `--vm-job-id`.
Pause and offload are mutually exclusive; `--vm-ram-file` requires offload.
Live resume continues the same Attempt. Ordinary persisted-Job suspend/resume
retain execution checkpoint capture/restoration. See the runtime README and
[CLI reference](../../docs/src/en/reference/cli.md#vm-instance-control) for
examples and endpoint/lifecycle limits.

Help, version and feature queries emit no startup logs. For execution commands,
`process.entry` and `cli.parsed` are emitted only after command parsing; these
markers do not measure the complete process-entry or argument-parsing overhead.

Host execution verifies the loaded executable once per process and pins that
inode. Request validation and the check immediately before worker spawn reuse
its digest only when the opened file's device, inode, size, nanosecond mtime and
ctime, mode and ownership match the verified image. Changes trigger a full hash;
metadata must remain stable across verification. Ancestor permissions, path
identity, peer credentials and listener compatibility remain checked. A warm
listener therefore normally needs no new content hash, while each new frontend
and worker still independently hash their loaded images; no caller-supplied hash
or persistent cross-process cache substitutes for that attestation.
The frontend's signal-only Tokio runtime uses one worker thread; execution
retains its separate worker runtime.

## Prepared VM rootfs

`pvisor-cache prepare-rootfs --rootfs DIR --base-store STORE` is a local operation,
independent of cache daemons, backends and registry access. The source must be
quiescent; preparation copies/verifies it and commits a persistent store pin
before printing `{schema_version, prepared_rootfs: {store, base: {id}}, rootfs}`.
Cache backend/location/read-only/image-store options conflict with this command.
The source and store must not contain one another. The private store must remain
outside guest-visible or writable workspace/stage/mount trees and must not be
modified manually. No prepared-entry removal command is provided.
Both local source and destination are checked against canonical Host authority
before initializing/copying, without contacting or changing a listener.

`pvisor run --vm --rootfs-base-store STORE --rootfs-base ID -- COMMAND` opens that
catalog entry in the ordinary Host request worker, before default host rootfs or
OCI preparation. Both options are required together and conflict with `--rootfs`,
configured image sources, host/container execution and JSON `--spec`. TOML can
use `[vm.prepared_rootfs]` with `store` and `base = { id = "..." }`; old configs
retain their defaults. Jobs persist the canonical selector and exact root path;
future workers must authenticate both rather than trust the path alone.

Warm opens verify the small record/seal and marker, then retain a GC lease in the
executor and native child; they do not reimport or fully hash the tree. Durable
catalog pins also preserve stopped Jobs after the preparer exits. Rootfs ownership
does not snapshot the workspace, promote OCI/lazy paths, or enable the default-off
lower-data/local-FLUSH experiments. Native execution checkpoint capture and
restore are unsupported for this source: the capability and Job admission report
that limit before capture, and no capture binding is installed. Workspace logical
checkpoints retain their independent behavior. See the runtime README for the
ownership and platform limits.

Successful final process-tree cleanup is idempotent; failed cleanup remains
retryable and descendant-only cleanup does not finalize ownership. Linux
observes the stop state of every discovered live process and thread before
rescanning descendants, instead of assuming a fixed delay quiesces forkers.
Pidfds wake descendant/root exit waits, and worker response events wake the
service immediately. The frontend still owns Child reaping and adopted orphan
cleanup; the service's Finish handshake and bounded escalation remain in place.
macOS retains birth-identity checks and polling during tree quiescence. These
changes remove redundant work in the source path; runtime latency improvements
require a new paired measurement.

## Prepared VM workspace

An explicit prepared workspace reads one fixed, owned generation while retaining
the live caller workspace as the apply destination. Prepare it outside the Run:

```bash
pvisor-cache prepare-workspace --workspace /opt/workspace-source --base-store /opt/workspace-bases
pvisor run --vm --rootfs-base-store /opt/rootfs-bases --rootfs-base ROOTFS_ID \
  --workspace-base-store /opt/workspace-bases --workspace-base WORKSPACE_ID -- /bin/sh
```

The preparation command is local, with the same backend-option conflicts and
canonical source/store/Host-authority checks as `prepare-rootfs`. It prints
`{schema_version, prepared_workspace: {store, base: {id}}, workspace}` after the
workspace-purpose catalog pin commits. A rootfs catalog record cannot substitute
for a workspace record. Preparation copies a quiescent source once; warm Runs
authenticate the catalog and seal without reimporting the source or auditing the
whole tree. The private store and generation must remain externally unmodified.

Run options require the workspace pair together. TOML uses
`[overlayfs.prepared_workspace]` with `store` and `base = { id = "..." }`.
`PreparedBase` is the common selector DTO; the existing `PreparedRootfs` Rust name
and `vm.prepared_rootfs` JSON/TOML representation remain compatible. A conflicting
CLI/config workspace selector fails. Jobs persist its canonical store/id for
independent later workers; serialized fields do not carry leases or ownership.

The first supported path requires a native VM, an authenticated prepared rootfs,
independent rootfs/workspace stores, one lower and fresh stage backing. It rejects
JSON `--spec`, host/container execution, staged compose layers, additional shares,
custom guest workspace positions, workspace checkpoint fork, and native execution
restore. The default live
workspace path is preserved inside the guest. Stores must be disjoint from that
live target, stage, root share, mutable VM backing, recording output and any
guest-visible sources, including canonical symlink aliases. Existing stage or
Run metadata is not permission to reopen or overwrite a previous Job.

The Host request worker reopens the workspace-purpose catalog and passes its
typed lease with the exact prepared-rootfs `Arc<VmExecutor>` to the runtime. Fresh
upper/compact-journal creation occurs under the Run lease. The native child
independently authenticates the physical seal and holds its own lease; it does
not recheck catalog purpose. Completion/cancellation notifications retain their
pre-teardown timing through a read-only runtime executor-output observer.

Reads use the selected generation even when live files change. Staged mutations
retain that generation as their baseline; manual apply targets the live workspace
and keeps normal content-conflict detection. Immutable lower does not mean the
live apply target is protected. No new VM cache, writeback or DAX setting is
enabled by this selector. Default Runs retain their existing live-workspace
semantics. These interface changes do not establish a performance result.

## Source layout

- `src/cli/`: Job commands, typed internal Host requests/listener/workers,
  safe Agent presets, terminal adapters and output rendering.
- `src/cli/features.rs`: runtime feature listing frontend; registry/settings
  remain in `pvisor::features`.
- `src/cli/cache.rs`: cache command parsing and rendering; cache storage,
  image preparation and authenticated server implementation remain in `pvisor`.
- `src/companions.rs`: trusted same-installation companion discovery/dispatch.
- `src/tui/`: native TUI PTY runtime, renderer, review panels and keymap,
  including Zellij attribution and the adapted border-glyph license.
- `src/bin/pvisor-replay.rs`: replay argument parsing and managed-run frontend;
  the independent replay engine remains in `pvisor-replay`.
- `src/bin/`: executable entry points.
- `tests/`: executable/frontend integration tests, including tests that combine
  runtime APIs with command execution. Runtime-only tests remain in `pvisor`.

The runtime README retains detailed execution, control, storage, service and
platform limits. Its command examples refer to these installed frontends.

## Build and validation

`gateway` forwards to `pvisor/gateway`; default builds keep Gateway optional.
Use `just test pvisor-cli` for application tests and `just test pvisor` for
runtime tests. Native VM and ignored environment-dependent gates retain their
existing prerequisites; compile checks are not real-guest validation.
